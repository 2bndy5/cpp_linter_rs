//! This submodule implements functionality exclusively specific to Github's REST API.

use std::{
    collections::HashMap,
    env,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Context, Result};
use reqwest::{Client, Method, Url};

use crate::{
    clang_tools::{clang_format::summarize_style, ClangVersions, ReviewComments},
    cli::{FeedbackInput, LinesChangedOnly},
    common_fs::{FileFilter, FileObj},
    git::parse_diff_from_buf,
    rest_api::{RestApiRateLimitHeaders, COMMENT_MARKER, USER_AGENT},
};

use super::{
    serde_structs::{
        FullReview, GithubChangedFile, PullRequestInfo, PushEventFiles, ReviewComment,
        ReviewDiffComment, ThreadComment, REVIEW_DISMISSAL,
    },
    GithubApiClient, RestApiClient,
};

impl GithubApiClient {
    /// Instantiate a [`GithubApiClient`] object.
    pub fn new() -> Result<Self> {
        let event_name = env::var("GITHUB_EVENT_NAME").unwrap_or(String::from("unknown"));
        let pull_request = {
            match event_name.as_str() {
                "pull_request" => {
                    // GITHUB_*** env vars cannot be overwritten in CI runners on GitHub.
                    let event_payload_path = env::var("GITHUB_EVENT_PATH")?;
                    // event payload JSON file can be overwritten/removed in CI runners
                    let file_buf = &mut String::new();
                    OpenOptions::new()
                        .read(true)
                        .open(event_payload_path.clone())?
                        .read_to_string(file_buf)
                        .with_context(|| {
                            format!("Failed to read event payload at {event_payload_path}")
                        })?;
                    let payload =
                        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                            file_buf,
                        )
                        .with_context(|| "Failed to deserialize event payload")?;
                    payload["number"].as_i64().unwrap_or(-1)
                }
                _ => -1,
            }
        };
        // GITHUB_*** env vars cannot be overwritten in CI runners on GitHub.
        let gh_api_url = env::var("GITHUB_API_URL").unwrap_or("https://api.github.com".to_string());
        let api_url = Url::parse(gh_api_url.as_str())?;

        Ok(GithubApiClient {
            client: Client::builder()
                .default_headers(Self::make_headers()?)
                .user_agent(USER_AGENT)
                .build()?,
            pull_request,
            event_name,
            api_url,
            repo: env::var("GITHUB_REPOSITORY").ok(),
            sha: env::var("GITHUB_SHA").ok(),
            debug_enabled: env::var("ACTIONS_STEP_DEBUG").is_ok_and(|val| &val == "true"),
            rate_limit_headers: RestApiRateLimitHeaders {
                reset: "x-ratelimit-reset".to_string(),
                remaining: "x-ratelimit-remaining".to_string(),
                retry: "retry-after".to_string(),
            },
        })
    }

    /// A way to get the list of changed files using REST API calls that employ a paginated response.
    ///
    /// This is a helper to [`Self::get_list_of_changed_files()`] but takes a formulated `url`
    /// endpoint based on the context of the triggering CI event.
    pub(super) async fn get_changed_files_paginated(
        &self,
        url: Url,
        file_filter: &FileFilter,
        lines_changed_only: &LinesChangedOnly,
    ) -> Result<Vec<FileObj>> {
        let mut url = Some(Url::parse_with_params(url.as_str(), &[("page", "1")])?);
        let mut files = vec![];
        while let Some(ref endpoint) = url {
            let request =
                Self::make_api_request(&self.client, endpoint.as_str(), Method::GET, None, None)?;
            let response = Self::send_api_request(
                self.client.clone(),
                request,
                self.rate_limit_headers.clone(),
                0,
            )
            .await
            .with_context(|| "Failed to get paginated list of changed files")?;
            url = Self::try_next_page(response.headers());
            let files_list = if self.event_name != "pull_request" {
                let json_value: PushEventFiles = serde_json::from_str(&response.text().await?)
                    .with_context(|| {
                        "Failed to deserialize list of changed files from json response"
                    })?;
                json_value.files
            } else {
                serde_json::from_str::<Vec<GithubChangedFile>>(&response.text().await?)
                    .with_context(|| {
                        "Failed to deserialize list of file changes from Pull Request event."
                    })?
            };
            for file in files_list {
                let ext = Path::new(&file.filename).extension().unwrap_or_default();
                if !file_filter
                    .extensions
                    .contains(&ext.to_string_lossy().to_string())
                {
                    continue;
                }
                if let Some(patch) = file.patch {
                    let diff = format!(
                        "diff --git a/{old} b/{new}\n--- a/{old}\n+++ b/{new}\n{patch}\n",
                        old = file.previous_filename.unwrap_or(file.filename.clone()),
                        new = file.filename,
                    );
                    if let Some(file_obj) =
                        parse_diff_from_buf(diff.as_bytes(), file_filter, lines_changed_only)
                            .first()
                    {
                        files.push(file_obj.to_owned());
                    }
                } else if file.changes == 0 {
                    // file may have been only renamed.
                    // include it in case files-changed-only is enabled.
                    files.push(FileObj::new(PathBuf::from(file.filename)));
                }
                // else changes are too big or we don't care
            }
        }
        Ok(files)
    }

    /// Append step summary to CI workflow's summary page.
    pub fn post_step_summary(&self, comment: &String) {
        if let Ok(gh_out) = env::var("GITHUB_STEP_SUMMARY") {
            // step summary MD file can be overwritten/removed in CI runners
            if let Ok(mut gh_out_file) = OpenOptions::new().append(true).open(gh_out) {
                if let Err(e) = writeln!(gh_out_file, "\n{}\n", comment) {
                    log::error!("Could not write to GITHUB_STEP_SUMMARY file: {}", e);
                }
            } else {
                log::error!("GITHUB_STEP_SUMMARY file could not be opened");
            }
        }
    }

    /// Post file annotations.
    pub fn post_annotations(&self, files: &[Arc<Mutex<FileObj>>], style: &str) {
        let style_guide = summarize_style(style);

        // iterate over clang-format advice and post annotations
        for file in files {
            let file = file.lock().unwrap();
            if let Some(format_advice) = &file.format_advice {
                // assemble a list of line numbers
                let mut lines = Vec::new();
                for replacement in &format_advice.replacements {
                    if !lines.contains(&replacement.line) {
                        lines.push(replacement.line);
                    }
                }
                // post annotation if any applicable lines were formatted
                if !lines.is_empty() {
                    println!(
                            "::notice file={name},title=Run clang-format on {name}::File {name} does not conform to {style_guide} style guidelines. (lines {line_set})",
                            name = &file.name.to_string_lossy().replace('\\', "/"),
                            line_set = lines.iter().map(|val| val.to_string()).collect::<Vec<_>>().join(","),
                        );
                }
            } // end format_advice iterations

            // iterate over clang-tidy advice and post annotations
            // The tidy_advice vector is parallel to the files vector; meaning it serves as a file filterer.
            // lines are already filter as specified to clang-tidy CLI.
            if let Some(tidy_advice) = &file.tidy_advice {
                for note in &tidy_advice.notes {
                    if note.filename == file.name.to_string_lossy().replace('\\', "/") {
                        println!(
                            "::{severity} file={file},line={line},title={file}:{line}:{cols} [{diag}]::{info}",
                            severity = if note.severity == *"note" { "notice".to_string() } else {note.severity.clone()},
                            file = note.filename,
                            line = note.line,
                            cols = note.cols,
                            diag = note.diagnostic,
                            info = note.rationale,
                        );
                    }
                }
            }
        }
    }

    /// Update existing comment or remove old comment(s) and post a new comment
    pub(super) async fn update_comment(
        &self,
        url: Url,
        comment: &String,
        no_lgtm: bool,
        is_lgtm: bool,
        update_only: bool,
    ) -> Result<()> {
        let comment_url = self
            .remove_bot_comments(&url, !update_only || (is_lgtm && no_lgtm))
            .await?;
        if !is_lgtm || !no_lgtm {
            let payload = HashMap::from([("body", comment)]);
            // log::debug!("payload body:\n{:?}", payload);
            let req_meth = if comment_url.is_some() {
                Method::PATCH
            } else {
                Method::POST
            };
            let request = Self::make_api_request(
                &self.client,
                comment_url.unwrap_or(url),
                req_meth,
                Some(serde_json::json!(&payload).to_string()),
                None,
            )?;
            match Self::send_api_request(
                self.client.clone(),
                request,
                self.rate_limit_headers.to_owned(),
                0,
            )
            .await
            {
                Ok(response) => {
                    Self::log_response(response, "Failed to post thread comment").await;
                }
                Err(e) => {
                    log::error!("Failed to post thread comment: {e:?}");
                }
            }
        }
        Ok(())
    }

    /// Remove thread comments previously posted by cpp-linter.
    async fn remove_bot_comments(&self, url: &Url, delete: bool) -> Result<Option<Url>> {
        let mut comment_url = None;
        let mut comments_url = Some(Url::parse_with_params(url.as_str(), &[("page", "1")])?);
        let repo = format!(
            "repos/{}{}/comments",
            // if we got here, then we know it is on a CI runner as self.repo should be known
            self.repo.as_ref().expect("Repo name unknown."),
            if self.event_name == "pull_request" {
                "/issues"
            } else {
                ""
            },
        );
        let base_comment_url = self.api_url.join(&repo).unwrap();
        while let Some(ref endpoint) = comments_url {
            let request =
                Self::make_api_request(&self.client, endpoint.as_str(), Method::GET, None, None)?;
            let result = Self::send_api_request(
                self.client.clone(),
                request,
                self.rate_limit_headers.to_owned(),
                0,
            )
            .await;
            match result {
                Err(e) => {
                    log::error!("Failed to get list of existing thread comments: {e:?}");
                    return Ok(comment_url);
                }
                Ok(response) => {
                    if !response.status().is_success() {
                        Self::log_response(
                            response,
                            "Failed to get list of existing thread comments",
                        )
                        .await;
                        return Ok(comment_url);
                    }
                    comments_url = Self::try_next_page(response.headers());
                    let payload =
                        serde_json::from_str::<Vec<ThreadComment>>(&response.text().await?);
                    match payload {
                        Err(e) => {
                            log::error!(
                                "Failed to deserialize list of existing thread comments: {e:?}"
                            );
                            continue;
                        }
                        Ok(payload) => {
                            for comment in payload {
                                if comment.body.starts_with(COMMENT_MARKER) {
                                    log::debug!(
                                        "Found cpp-linter comment id {} from user {} ({})",
                                        comment.id,
                                        comment.user.login,
                                        comment.user.id,
                                    );
                                    let this_comment_url = Url::parse(
                                        format!("{base_comment_url}/{}", comment.id).as_str(),
                                    )?;
                                    if delete || comment_url.is_some() {
                                        // if not updating: remove all outdated comments
                                        // if updating: remove all outdated comments except the last one

                                        // use last saved comment_url (if not None) or current comment url
                                        let del_url = if let Some(last_url) = &comment_url {
                                            last_url
                                        } else {
                                            &this_comment_url
                                        };
                                        let req = Self::make_api_request(
                                            &self.client,
                                            del_url.as_str(),
                                            Method::DELETE,
                                            None,
                                            None,
                                        )?;
                                        match Self::send_api_request(
                                            self.client.clone(),
                                            req,
                                            self.rate_limit_headers.to_owned(),
                                            0,
                                        )
                                        .await
                                        {
                                            Ok(result) => {
                                                if !result.status().is_success() {
                                                    Self::log_response(
                                                        result,
                                                        "Failed to delete old thread comment",
                                                    )
                                                    .await;
                                                }
                                            }
                                            Err(e) => {
                                                log::error!(
                                                    "Failed to delete old thread comment: {e:?}"
                                                )
                                            }
                                        }
                                    }
                                    if !delete {
                                        comment_url = Some(this_comment_url)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(comment_url)
    }

    /// Post a PR review with code suggestions.
    ///
    /// Note: `--no-lgtm` is applied when nothing is suggested.
    pub async fn post_review(
        &self,
        files: &[Arc<Mutex<FileObj>>],
        feedback_input: &FeedbackInput,
        clang_versions: &ClangVersions,
    ) -> Result<()> {
        let url = self
            .api_url
            .join("repos/")?
            .join(
                format!(
                    "{}/",
                    // if we got here, then we know self.repo should be known
                    self.repo.as_ref().ok_or(anyhow!("Repo name unknown"))?
                )
                .as_str(),
            )?
            .join("pulls/")?
            // if we got here, then we know that it is a self.pull_request is a valid value
            .join(self.pull_request.to_string().as_str())?;
        let request = Self::make_api_request(&self.client, url.as_str(), Method::GET, None, None)?;
        let response = Self::send_api_request(
            self.client.clone(),
            request,
            self.rate_limit_headers.clone(),
            0,
        );

        let url = Url::parse(format!("{}/", url).as_str())?.join("reviews")?;
        match response.await {
            Ok(response) => {
                match serde_json::from_str::<PullRequestInfo>(&response.text().await?) {
                    Err(e) => {
                        log::error!("Failed to deserialize PR info: {e:?}");
                        return self.dismiss_outdated_reviews(url, None).await;
                    }
                    Ok(pr_info) => {
                        if pr_info.draft || pr_info.state != "open" {
                            return self.dismiss_outdated_reviews(url, None).await;
                        }
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to get PR info from {e:?}");
                return self.dismiss_outdated_reviews(url, None).await;
            }
        }

        let summary_only = ["true", "on", "1"].contains(
            &env::var("CPP_LINTER_PR_REVIEW_SUMMARY_ONLY")
                .unwrap_or("false".to_string())
                .as_str(),
        );

        let mut review_comments = ReviewComments::default();
        for file in files {
            let file = file.lock().unwrap();
            file.make_suggestions_from_patch(&mut review_comments, summary_only)?;
        }

        let ignored_reviews = if summary_only {
            vec![]
        } else {
            self.check_reused_comments(&mut review_comments, feedback_input.delete_review_comments)
                .await?
        };
        let hiding = self.hide_outdated_reviews(url.clone(), ignored_reviews.clone());
        if review_comments.comments.is_empty() && !ignored_reviews.is_empty() {
            log::info!("Using previous review as nothing new was found");
            return hiding.await;
        }
        let dismissal = self.dismiss_outdated_reviews(url.clone(), Some(ignored_reviews));

        let has_no_changes =
            review_comments.full_patch[0].is_empty() && review_comments.full_patch[1].is_empty();
        if has_no_changes && feedback_input.no_lgtm {
            log::debug!("Not posting an approved review because `no-lgtm` is true");
            hiding.await?;
            return dismissal.await;
        }

        let mut payload = FullReview {
            event: if feedback_input.passive_reviews {
                String::from("COMMENT")
            } else if has_no_changes && review_comments.comments.is_empty() {
                // if patches have no changes AND there are no comments about clang-tidy diagnostics
                String::from("APPROVE")
            } else {
                String::from("REQUEST_CHANGES")
            },
            body: String::new(),
            comments: vec![],
        };
        payload.body = review_comments.summarize(clang_versions);
        if !summary_only {
            payload.comments = {
                let mut comments = vec![];
                for comment in review_comments.comments {
                    comments.push(ReviewDiffComment::from(comment));
                }
                comments
            };
        }
        let request = Self::make_api_request(
            &self.client,
            url.clone(),
            Method::POST,
            Some(
                serde_json::to_string(&payload)
                    .with_context(|| "Failed to serialize PR review to json string")?,
            ),
            None,
        )?;
        hiding.await?;
        dismissal.await?;
        match Self::send_api_request(
            self.client.clone(),
            request,
            self.rate_limit_headers.clone(),
            0,
        )
        .await
        {
            Ok(response) => {
                if !response.status().is_success() {
                    Self::log_response(response, "Failed to post a new PR review").await;
                }
            }
            Err(e) => {
                log::error!("Failed to post a new PR review: {e:?}");
            }
        }
        Ok(())
    }

    /// Dismiss any outdated reviews generated by cpp-linter.
    async fn dismiss_outdated_reviews(
        &self,
        url: Url,
        ignored_reviews: Option<Vec<String>>,
    ) -> Result<()> {
        let mut url_ = Some(Url::parse_with_params(url.as_str(), [("page", "1")])?);
        while let Some(ref endpoint) = url_ {
            let request =
                Self::make_api_request(&self.client, endpoint.as_str(), Method::GET, None, None)?;
            let result = Self::send_api_request(
                self.client.clone(),
                request,
                self.rate_limit_headers.clone(),
                0,
            )
            .await;
            match result {
                Err(e) => {
                    log::error!("Failed to get a list of existing PR reviews: {e:?}");
                    return Ok(());
                }
                Ok(response) => {
                    if !response.status().is_success() {
                        Self::log_response(response, "Failed to get a list of existing PR reviews")
                            .await;
                        return Ok(());
                    }
                    url_ = Self::try_next_page(response.headers());
                    match serde_json::from_str::<Vec<ReviewComment>>(&response.text().await?) {
                        Err(e) => {
                            log::error!("Unable to serialize JSON about review comments: {e:?}");
                            return Ok(());
                        }
                        Ok(payload) => {
                            for review in payload {
                                if let Some(body) = &review.body {
                                    if body.starts_with(COMMENT_MARKER)
                                        && !(["PENDING", "DISMISSED"]
                                            .contains(&review.state.as_str()))
                                        && (ignored_reviews.is_none()
                                            || ignored_reviews
                                                .as_ref()
                                                .is_some_and(|r| r.contains(&review.node_id)))
                                    {
                                        // dismiss outdated review
                                        if let Ok(dismiss_url) = url.join(
                                            format!("reviews/{}/dismissals", review.id).as_str(),
                                        ) {
                                            if let Ok(req) = Self::make_api_request(
                                                &self.client,
                                                dismiss_url,
                                                Method::PUT,
                                                Some(REVIEW_DISMISSAL.to_string()),
                                                None,
                                            ) {
                                                match Self::send_api_request(
                                                    self.client.clone(),
                                                    req,
                                                    self.rate_limit_headers.clone(),
                                                    0,
                                                )
                                                .await
                                                {
                                                    Ok(result) => {
                                                        if !result.status().is_success() {
                                                            Self::log_response(
                                                                result,
                                                                "Failed to dismiss outdated review",
                                                            )
                                                            .await;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        log::error!(
                                                            "Failed to dismiss outdated review: {e:}"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

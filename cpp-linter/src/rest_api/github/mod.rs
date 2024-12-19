//! This module holds functionality specific to using Github's REST API.
//!
//! In the root module, we just implement the RestApiClient trait.
//! In other (private) submodules we implement behavior specific to Github's REST API.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

// non-std crates
use anyhow::{Context, Result};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client, Method, Url,
};

// project specific modules/crates
use super::{RestApiClient, RestApiRateLimitHeaders};
use crate::clang_tools::clang_format::tally_format_advice;
use crate::clang_tools::clang_tidy::tally_tidy_advice;
use crate::clang_tools::ClangVersions;
use crate::cli::{FeedbackInput, LinesChangedOnly, ThreadComments};
use crate::common_fs::{FileFilter, FileObj};
use crate::git::{get_diff, open_repo, parse_diff, parse_diff_from_buf};

// private submodules.
mod serde_structs;
mod specific_api;
use serde_structs::{GithubChangedFile, PushEventFiles};
mod graphql;

/// A structure to work with Github REST API.
pub struct GithubApiClient {
    /// The HTTP request client to be used for all REST API calls.
    client: Client,

    /// The CI run's event payload from the webhook that triggered the workflow.
    pull_request: i64,

    /// The name of the event that was triggered when running cpp_linter.
    pub event_name: String,

    /// The value of the `GITHUB_API_URL` environment variable.
    api_url: Url,

    /// The value of the `GITHUB_REPOSITORY` environment variable.
    repo: Option<String>,

    /// The value of the `GITHUB_SHA` environment variable.
    sha: Option<String>,

    /// The value of the `ACTIONS_STEP_DEBUG` environment variable.
    pub debug_enabled: bool,

    /// The response header names that describe the rate limit status.
    rate_limit_headers: RestApiRateLimitHeaders,
}

// implement the RestApiClient trait for the GithubApiClient
impl RestApiClient for GithubApiClient {
    fn set_exit_code(
        &self,
        checks_failed: u64,
        format_checks_failed: Option<u64>,
        tidy_checks_failed: Option<u64>,
    ) -> u64 {
        if let Ok(gh_out) = env::var("GITHUB_OUTPUT") {
            if let Ok(mut gh_out_file) = OpenOptions::new().append(true).open(gh_out) {
                for (prompt, value) in [
                    ("checks-failed", Some(checks_failed)),
                    ("format-checks-failed", format_checks_failed),
                    ("tidy-checks-failed", tidy_checks_failed),
                ] {
                    if let Err(e) = writeln!(gh_out_file, "{prompt}={}", value.unwrap_or(0),) {
                        log::error!("Could not write to GITHUB_OUTPUT file: {}", e);
                        break;
                    }
                }
                if let Err(e) = gh_out_file.flush() {
                    log::debug!("Failed to flush buffer to GITHUB_OUTPUT file: {e:?}");
                }
            } else {
                log::debug!("GITHUB_OUTPUT file could not be opened");
            }
        }
        log::info!(
            "{} clang-format-checks-failed",
            format_checks_failed.unwrap_or(0)
        );
        log::info!(
            "{} clang-tidy-checks-failed",
            tidy_checks_failed.unwrap_or(0)
        );
        log::info!("{checks_failed} checks-failed");
        checks_failed
    }

    /// This prints a line to indicate the beginning of a related group of log statements.
    fn start_log_group(&self, name: String) {
        log::info!(target: "CI_LOG_GROUPING", "::group::{}", name);
    }

    /// This prints a line to indicate the ending of a related group of log statements.
    fn end_log_group(&self) {
        log::info!(target: "CI_LOG_GROUPING", "::endgroup::");
    }

    fn make_headers() -> Result<HeaderMap<HeaderValue>> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Accept",
            HeaderValue::from_str("application/vnd.github.raw+json")?,
        );
        if let Ok(token) = env::var("GITHUB_TOKEN") {
            log::debug!("Using auth token from GITHUB_TOKEN environment variable");
            let mut val = HeaderValue::from_str(format!("token {token}").as_str())?;
            val.set_sensitive(true);
            headers.insert(AUTHORIZATION, val);
        }
        Ok(headers)
    }

    async fn get_list_of_changed_files(
        &self,
        file_filter: &FileFilter,
        lines_changed_only: &LinesChangedOnly,
    ) -> Result<Vec<FileObj>> {
        if env::var("CI").is_ok_and(|val| val.as_str() == "true")
            && self.repo.is_some()
            && self.sha.is_some()
        {
            // get diff from Github REST API
            let is_pr = self.event_name == "pull_request";
            let pr = self.pull_request.to_string();
            let sha = self.sha.clone().unwrap();
            let url = self
                .api_url
                .join("repos/")?
                .join(format!("{}/", self.repo.as_ref().unwrap()).as_str())?
                .join(if is_pr { "pulls/" } else { "commits/" })?
                .join(if is_pr { pr.as_str() } else { sha.as_str() })?;
            let mut diff_header = HeaderMap::new();
            diff_header.insert("Accept", "application/vnd.github.diff".parse()?);
            log::debug!("Getting file changes from {}", url.as_str());
            let request = Self::make_api_request(
                &self.client,
                url.as_str(),
                Method::GET,
                None,
                Some(diff_header),
            )?;
            let response = Self::send_api_request(
                self.client.clone(),
                request,
                self.rate_limit_headers.to_owned(),
                0,
            )
            .await
            .with_context(|| "Failed to get list of changed files.")?;
            if response.status().is_success() {
                Ok(parse_diff_from_buf(
                    &response.bytes().await?,
                    file_filter,
                    lines_changed_only,
                ))
            } else {
                let endpoint = if is_pr {
                    Url::parse(format!("{}/files", url.as_str()).as_str())?
                } else {
                    url
                };
                Self::log_response(response, "Failed to get full diff for event").await;
                log::debug!("Trying paginated request to {}", endpoint.as_str());
                self.get_changed_files_paginated(endpoint, file_filter, lines_changed_only)
                    .await
            }
        } else {
            // get diff from libgit2 API
            let repo = open_repo(".").with_context(|| {
                "Please ensure the repository is checked out before running cpp-linter."
            })?;
            let list = parse_diff(&get_diff(&repo)?, file_filter, lines_changed_only);
            Ok(list)
        }
    }

    async fn post_feedback(
        &self,
        files: &[Arc<Mutex<FileObj>>],
        feedback_inputs: FeedbackInput,
        clang_versions: ClangVersions,
    ) -> Result<u64> {
        let tidy_checks_failed = tally_tidy_advice(files);
        let format_checks_failed = tally_format_advice(files);
        let mut comment = None;

        if feedback_inputs.file_annotations {
            self.post_annotations(files, feedback_inputs.style.as_str());
        }
        if feedback_inputs.step_summary {
            comment = Some(Self::make_comment(
                files,
                format_checks_failed,
                tidy_checks_failed,
                &clang_versions,
                None,
            ));
            self.post_step_summary(comment.as_ref().unwrap());
        }
        self.set_exit_code(
            format_checks_failed + tidy_checks_failed,
            Some(format_checks_failed),
            Some(tidy_checks_failed),
        );

        if feedback_inputs.thread_comments != ThreadComments::Off {
            // post thread comment for PR or push event
            if comment.as_ref().is_some_and(|c| c.len() > 65535) || comment.is_none() {
                comment = Some(Self::make_comment(
                    files,
                    format_checks_failed,
                    tidy_checks_failed,
                    &clang_versions,
                    Some(65535),
                ));
            }
            if let Some(repo) = &self.repo {
                let is_pr = self.event_name == "pull_request";
                let pr = self.pull_request.to_string() + "/";
                let sha = self.sha.clone().unwrap() + "/";
                let comments_url = self
                    .api_url
                    .join("repos/")?
                    .join(format!("{}/", repo).as_str())?
                    .join(if is_pr { "issues/" } else { "commits/" })?
                    .join(if is_pr { pr.as_str() } else { sha.as_str() })?
                    .join("comments")?;

                self.update_comment(
                    comments_url,
                    &comment.unwrap(),
                    feedback_inputs.no_lgtm,
                    format_checks_failed + tidy_checks_failed == 0,
                    feedback_inputs.thread_comments == ThreadComments::Update,
                )
                .await?;
            }
        }
        if self.event_name == "pull_request"
            && (feedback_inputs.tidy_review || feedback_inputs.format_review)
        {
            self.post_review(files, &feedback_inputs, &clang_versions)
                .await?;
        }
        Ok(format_checks_failed + tidy_checks_failed)
    }
}

#[cfg(test)]
mod test {
    use std::{
        default::Default,
        env,
        io::Read,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use regex::Regex;
    use tempfile::{tempdir, NamedTempFile};

    use super::GithubApiClient;
    use crate::{
        clang_tools::{
            clang_format::{FormatAdvice, Replacement},
            clang_tidy::{TidyAdvice, TidyNotification},
            ClangVersions,
        },
        cli::{FeedbackInput, LinesChangedOnly},
        common_fs::{FileFilter, FileObj},
        logger,
        rest_api::{RestApiClient, USER_OUTREACH},
    };

    // ************************* tests for step-summary and output variables

    async fn create_comment(
        is_lgtm: bool,
        fail_gh_out: bool,
        fail_summary: bool,
    ) -> (String, String) {
        let tmp_dir = tempdir().unwrap();
        let rest_api_client = GithubApiClient::new().unwrap();
        logger::try_init();
        if env::var("ACTIONS_STEP_DEBUG").is_ok_and(|var| var == "true") {
            assert!(rest_api_client.debug_enabled);
            log::set_max_level(log::LevelFilter::Debug);
        }
        let mut files = vec![];
        if !is_lgtm {
            for _i in 0..65535 {
                let filename = String::from("tests/demo/demo.cpp");
                let mut file = FileObj::new(PathBuf::from(&filename));
                let notes = vec![TidyNotification {
                    filename,
                    line: 0,
                    cols: 0,
                    severity: String::from("note"),
                    rationale: String::from("A test dummy rationale"),
                    diagnostic: String::from("clang-diagnostic-warning"),
                    suggestion: vec![],
                    fixed_lines: vec![],
                }];
                file.tidy_advice = Some(TidyAdvice {
                    notes,
                    patched: None,
                });
                file.format_advice = Some(FormatAdvice {
                    replacements: vec![Replacement { offset: 0, line: 1 }],
                    patched: None,
                });
                files.push(Arc::new(Mutex::new(file)));
            }
        }
        let feedback_inputs = FeedbackInput {
            style: if is_lgtm {
                String::new()
            } else {
                String::from("file")
            },
            step_summary: true,
            ..Default::default()
        };
        let mut step_summary_path = NamedTempFile::new_in(tmp_dir.path()).unwrap();
        env::set_var(
            "GITHUB_STEP_SUMMARY",
            if fail_summary {
                Path::new("not-a-file.txt")
            } else {
                step_summary_path.path()
            },
        );
        let mut gh_out_path = NamedTempFile::new_in(tmp_dir.path()).unwrap();
        env::set_var(
            "GITHUB_OUTPUT",
            if fail_gh_out {
                Path::new("not-a-file.txt")
            } else {
                gh_out_path.path()
            },
        );
        let clang_versions = ClangVersions {
            format_version: Some("x.y.z".to_string()),
            tidy_version: Some("x.y.z".to_string()),
        };
        rest_api_client
            .post_feedback(&files, feedback_inputs, clang_versions)
            .await
            .unwrap();
        let mut step_summary_content = String::new();
        step_summary_path
            .read_to_string(&mut step_summary_content)
            .unwrap();
        if !fail_summary {
            assert!(&step_summary_content.contains(USER_OUTREACH));
        }
        let mut gh_out_content = String::new();
        gh_out_path.read_to_string(&mut gh_out_content).unwrap();
        if !fail_gh_out {
            assert!(gh_out_content.starts_with("checks-failed="));
        }
        (step_summary_content, gh_out_content)
    }

    #[tokio::test]
    async fn check_comment_concerns() {
        let (comment, gh_out) = create_comment(false, false, false).await;
        assert!(&comment.contains(":warning:\nSome files did not pass the configured checks!\n"));
        let fmt_pattern = Regex::new(r"format-checks-failed=(\d+)\n").unwrap();
        let tidy_pattern = Regex::new(r"tidy-checks-failed=(\d+)\n").unwrap();
        for pattern in [fmt_pattern, tidy_pattern] {
            let number = pattern
                .captures(&gh_out)
                .expect("found no number of checks-failed")
                .get(1)
                .unwrap()
                .as_str()
                .parse::<u64>()
                .unwrap();
            assert!(number > 0);
        }
    }

    #[tokio::test]
    async fn check_comment_lgtm() {
        env::set_var("ACTIONS_STEP_DEBUG", "true");
        let (comment, gh_out) = create_comment(true, false, false).await;
        assert!(comment.contains(":heavy_check_mark:\nNo problems need attention."));
        assert_eq!(
            gh_out,
            "checks-failed=0\nformat-checks-failed=0\ntidy-checks-failed=0\n"
        );
    }

    #[tokio::test]
    async fn fail_gh_output() {
        env::set_var("ACTIONS_STEP_DEBUG", "true");
        let (comment, gh_out) = create_comment(true, true, false).await;
        assert!(&comment.contains(":heavy_check_mark:\nNo problems need attention."));
        assert!(gh_out.is_empty());
    }

    #[tokio::test]
    async fn fail_gh_summary() {
        env::set_var("ACTIONS_STEP_DEBUG", "true");
        let (comment, gh_out) = create_comment(true, false, true).await;
        assert!(comment.is_empty());
        assert_eq!(
            gh_out,
            "checks-failed=0\nformat-checks-failed=0\ntidy-checks-failed=0\n"
        );
    }

    #[tokio::test]
    async fn fail_get_local_diff() {
        env::set_var("CI", "false");
        let tmp_dir = tempdir().unwrap();
        env::set_current_dir(tmp_dir.path()).unwrap();
        let rest_client = GithubApiClient::new().unwrap();
        let files = rest_client
            .get_list_of_changed_files(&FileFilter::new(&[], vec![]), &LinesChangedOnly::Off)
            .await;
        assert!(files.is_err())
    }
}

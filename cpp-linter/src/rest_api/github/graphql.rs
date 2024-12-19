use std::collections::HashMap;

use anyhow::{anyhow, Result};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::json;

use crate::{
    clang_tools::ReviewComments,
    rest_api::{RestApiClient, COMMENT_MARKER},
};

use super::{serde_structs::ReviewComment, GithubApiClient};

const QUERY_REVIEW_COMMENTS: &str = r#"query($owner: String!, $name: String!, $number: Int!, $afterThread: String, $afterComment: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(last: 100, after: $afterThread) {
        nodes {
          id
          isResolved
          isCollapsed
          comments(first: 100, after: $afterComment) {
            nodes {
              id
              body
              path
              line
              startLine
              originalLine
              originalStartLine
              pullRequestReview {
                id
                isMinimized
              }
            }
            pageInfo {
              endCursor
              hasNextPage
            }
          }
        }
        pageInfo {
          endCursor
          hasNextPage
        }
      }
    }
  }
}"#;

const RESOLVE_REVIEW_COMMENT: &str = r#"mutation($id: ID!) {
  resolveReviewThread(input: {threadId: $id, clientMutationId: "github-actions"}) {
    thread {
      id
    }
  }
}"#;

const DELETE_REVIEW_COMMENT: &str = r#"mutation($id: ID!) {
  deletePullRequestReviewComment(input: {id: $id, clientMutationId: "github-actions"}) {
    pullRequestReviewComment {
      id
    }
  }
}"#;

const HIDE_REVIEW_COMMENT: &str = r#"mutation($subjectId: ID!) {
  minimizeComment(input: {classifier:OUTDATED, subjectId: $subjectId, clientMutationId: "github-actions"}) {
    minimizedComment {
      isMinimized
    }
  }
}"#;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct ThreadInfo {
    pub id: String,
    pub is_collapsed: bool,
    pub is_resolved: bool,
}

pub struct ReviewThread {
    pub info: ThreadInfo,
    pub comments: Vec<QueryResponseReviewThreadComment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_pg: bool,
    end_cursor: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponsePrReview {
    pub id: String,
    pub is_minimized: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponseReviewThreadComment {
    pub id: String,
    pub body: String,
    pub path: String,
    pub line: Option<i64>,
    pub start_line: Option<i64>,
    pub original_line: i64,
    pub original_start_line: Option<i64>,
    pub pull_request_review: QueryResponsePrReview,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponseReviewThreadComments {
    pub nodes: Vec<QueryResponseReviewThreadComment>,
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponseReviewThread {
    pub id: String,
    pub is_collapsed: bool,
    pub is_resolved: bool,
    pub comments: QueryResponseReviewThreadComments,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponseReviewThreads {
    nodes: Vec<QueryResponseReviewThread>,
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponsePr {
    review_threads: QueryResponseReviewThreads,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryResponseRepo {
    pull_request: QueryResponsePr,
}

#[derive(Debug, Deserialize)]
struct QueryResponseData {
    repository: QueryResponseRepo,
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    pub data: QueryResponseData,
}

impl GithubApiClient {
    /// Creates the list existing review thread comments to close.
    ///
    /// Set `no_dismissed` is `true` to ignore any already dismissed comments.
    async fn get_existing_review_comments(&self, no_dismissed: bool) -> Result<Vec<ReviewThread>> {
        let mut found_threads: HashMap<ThreadInfo, Vec<QueryResponseReviewThreadComment>> =
            HashMap::new();
        let repo_name_split = self
            .repo
            .clone()
            .ok_or(anyhow!("Repository name unknown"))?
            .split("/")
            .map(|d| d.to_string())
            .collect::<Vec<String>>();
        let mut after_thread = None;
        let mut after_comment = None;
        let mut has_next_page = true;
        while has_next_page {
            let variables = json!({
                "owner": repo_name_split[0],
                "name": repo_name_split[1],
                "number": self.pull_request,
                "afterThread": after_thread,
                "afterComment": after_comment,
            });
            let req = Self::make_api_request(
                &self.client,
                format!("{}/graphql", self.api_url.as_str()),
                Method::POST,
                Some(json!({"query": QUERY_REVIEW_COMMENTS, "variables": variables}).to_string()),
                None,
            )?;
            let response = Self::send_api_request(
                self.client.clone(),
                req,
                self.rate_limit_headers.clone(),
                0,
            )
            .await;
            match response {
                Err(_) => {
                    log::error!("Failed to get existing review thread comments");
                    break;
                }
                Ok(response) => {
                    match serde_json::from_str::<QueryResponse>(response.text().await?.as_str()) {
                        Err(e) => {
                            log::error!(
                                "GraphQL response was malformed. Failed to deserialize payload: {e}"
                            );
                            break;
                        }
                        Ok(payload) => {
                            let threads_data = payload.data.repository.pull_request.review_threads;
                            let thread_pg_info = threads_data.page_info;
                            for thread in threads_data.nodes {
                                let comment_data = &thread.comments;
                                let comment_pg_info = &comment_data.page_info;
                                let thread_info = ThreadInfo {
                                    id: thread.id.clone(),
                                    is_collapsed: thread.is_collapsed,
                                    is_resolved: thread.is_resolved,
                                };
                                for comment in &comment_data.nodes {
                                    if comment.body.starts_with(COMMENT_MARKER)
                                        && (!no_dismissed
                                            || (!thread.is_resolved && !thread.is_collapsed))
                                    {
                                        if found_threads.contains_key(&thread_info) {
                                            found_threads
                                                .get_mut(&thread_info)
                                                .unwrap()
                                                .push(comment.clone());
                                        } else {
                                            found_threads
                                                .insert(thread_info.clone(), vec![comment.clone()]);
                                        }
                                    }
                                }
                                after_comment = if comment_pg_info.has_next_pg {
                                    Some(comment_pg_info.end_cursor.clone())
                                } else {
                                    None
                                };
                            }
                            if after_comment.is_none() {
                                if thread_pg_info.has_next_pg {
                                    has_next_page = false;
                                } else {
                                    after_thread = Some(thread_pg_info.end_cursor);
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut result = vec![];
        for (info, comments) in found_threads {
            result.push(ReviewThread { info, comments });
        }
        Ok(result)
    }

    /// This will sort through the threads of PR reviews and return a list of
    /// bot comments to be kept.
    ///
    /// This will also resolve (or delete if `delete_review_comments` is `true`)
    /// any outdated unresolved comment.
    pub(super) async fn check_reused_comments(
        &self,
        review_comments: &mut ReviewComments,
        delete_review_comments: bool,
    ) -> Result<Vec<String>> {
        let mut ignored_reviews = vec![];
        let found_threads = self
            .get_existing_review_comments(!delete_review_comments)
            .await?;
        if found_threads.is_empty() {
            return Ok(ignored_reviews);
        }

        // Keep already posted comments if they match new ones
        let mut existing_review_comments = vec![];
        for thread in &found_threads {
            for comment in &thread.comments {
                let line_start = comment
                    .start_line
                    .unwrap_or(comment.original_start_line.unwrap_or(-1));
                let line_end = comment.line.unwrap_or(comment.original_line);
                let mut found = false;
                for suggestion in &review_comments.comments {
                    if suggestion.path == comment.path
                        && suggestion.line_start as i64 == line_start
                        && suggestion.line_end as i64 == line_end
                        && format!("{COMMENT_MARKER}{}", suggestion.suggestion) == comment.body
                        && !existing_review_comments.contains(suggestion)
                        && !thread.info.is_resolved
                        && !thread.info.is_collapsed
                        && !comment.pull_request_review.is_minimized
                    {
                        log::info!(
                            "Using existing review comment: path='{}', line_start='{line_start}', line_end='{line_end}'",
                            comment.path,
                        );
                        ignored_reviews.push(comment.pull_request_review.id.clone());
                        existing_review_comments.push(suggestion.clone());
                        found = true;
                        break;
                    }
                }
                if !found {
                    self.close_review_comment(
                        if delete_review_comments {
                            comment.id.clone()
                        } else {
                            thread.info.id.clone()
                        },
                        delete_review_comments,
                    )
                    .await?;
                }
            }
        }
        review_comments.remove_reused_suggestions(existing_review_comments);
        Ok(ignored_reviews)
    }

    /// Resolve or Delete an existing review thread comment.
    ///
    /// The `thread_id` is the Review thread's ID for the conversation to close.
    /// The `comment_id` is the comment ID of the comment within the requested thread to close.
    /// Pass `delete` as `true` to delete the review comment, `false` to set it as resolved.
    async fn close_review_comment(&self, id: String, delete: bool) -> Result<()> {
        let mutation = if delete {
            DELETE_REVIEW_COMMENT
        } else {
            RESOLVE_REVIEW_COMMENT
        };
        let request = Self::make_api_request(
            &self.client,
            format!("{}/graphql", self.api_url),
            Method::POST,
            Some(json!({"query": mutation, "variables": { "id": id }}).to_string()),
            None,
        )?;
        if let Ok(response) = Self::send_api_request(
            self.client.clone(),
            request,
            self.rate_limit_headers.clone(),
            0,
        )
        .await
        {
            log::debug!(
                "{} review comment {} ({}: {})",
                if delete { "Delete" } else { "Resolve" },
                if response.status().is_success() {
                    "failed"
                } else {
                    "succeeded"
                },
                if delete { "comment_id" } else { "thread_id" },
                id,
            );
        }
        Ok(())
    }

    /// Hide all review comments that were previously created by cpp-linter
    ///
    /// The `ignored_reviews` parameter is List of review comments to keep displayed.
    pub(super) async fn hide_outdated_reviews(
        &self,
        url: Url,
        ignored_reviews: Vec<String>,
    ) -> Result<()> {
        let mut next_page = Some(Url::parse_with_params(url.as_str(), [("page", "1")])?);
        while let Some(url) = next_page {
            let request = Self::make_api_request(&self.client, url, Method::GET, None, None)?;
            let response = Self::send_api_request(
                self.client.clone(),
                request,
                self.rate_limit_headers.clone(),
                0,
            )
            .await;
            match response {
                Err(_) => {
                    log::warn!("Failed to get list of existing reviews");
                    return Ok(());
                }
                Ok(response) => {
                    next_page = Self::try_next_page(response.headers());
                    let reviews = serde_json::from_str::<Vec<ReviewComment>>(
                        response.text().await?.as_str(),
                    )?;
                    for review in reviews {
                        if review
                            .body
                            .as_ref()
                            .is_some_and(|b| b.starts_with(COMMENT_MARKER))
                            && !ignored_reviews.contains(&review.node_id)
                        {
                            let req = Self::make_api_request(
                                &self.client,
                                format!("{}/graphql", self.api_url),
                                Method::POST,
                                Some(json!({"query`": HIDE_REVIEW_COMMENT, "variables": {"subjectId": review.node_id}}).to_string()),
                                None
                            )?;
                            if let Ok(res) = Self::send_api_request(
                                self.client.clone(),
                                req,
                                self.rate_limit_headers.clone(),
                                0,
                            )
                            .await
                            {
                                log::debug!(
                                    "Minimized review comment: {} (node_id: {})",
                                    res.status().is_success(),
                                    review.node_id,
                                )
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

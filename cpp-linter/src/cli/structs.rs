use std::{fmt::Display, path::PathBuf};

use clap::ArgMatches;

use super::convert_extra_arg_val;
use crate::{clang_tools::clang_tidy::CompilationUnit, common_fs::FileFilter};

/// An enum to describe `--lines-changed-only` CLI option's behavior.
#[derive(PartialEq, Clone, Debug, Default)]
pub enum LinesChangedOnly {
    /// All lines are scanned
    #[default]
    Off,
    /// Only lines in the diff are scanned
    Diff,
    /// Only lines in the diff with additions are scanned.
    On,
}

impl LinesChangedOnly {
    fn from_string(val: &str) -> LinesChangedOnly {
        match val {
            "true" | "on" | "1" => LinesChangedOnly::On,
            "diff" => LinesChangedOnly::Diff,
            _ => LinesChangedOnly::Off,
        }
    }

    pub fn is_change_valid(&self, added_lines: bool, diff_chunks: bool) -> bool {
        match self {
            LinesChangedOnly::Off => true,
            LinesChangedOnly::Diff => diff_chunks,
            LinesChangedOnly::On => added_lines,
        }
    }
}

impl Display for LinesChangedOnly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinesChangedOnly::Off => write!(f, "false"),
            LinesChangedOnly::Diff => write!(f, "diff"),
            LinesChangedOnly::On => write!(f, "true"),
        }
    }
}

/// A structure to contain parsed CLI options.
pub struct Cli {
    pub version: String,
    pub verbosity: bool,
    pub extensions: Vec<String>,
    pub repo_root: String,
    pub lines_changed_only: LinesChangedOnly,
    pub files_changed_only: bool,
    pub ignore: Vec<String>,
    pub style: String,
    pub ignore_format: Option<Vec<String>>,
    pub ignore_tidy: Option<Vec<String>>,
    pub tidy_checks: String,
    pub database: Option<PathBuf>,
    pub extra_arg: Vec<String>,
    pub thread_comments: ThreadComments,
    pub no_lgtm: bool,
    pub step_summary: bool,
    pub file_annotations: bool,
    pub not_ignored: Option<Vec<String>>,
    pub tidy_review: bool,
    pub format_review: bool,
    pub passive_reviews: bool,
    pub delete_review_comments: bool,
}

impl From<&ArgMatches> for Cli {
    /// Construct a [`Cli`] instance from a [`ArgMatches`] instance (after options are parsed from CLI).
    fn from(args: &ArgMatches) -> Self {
        let ignore = args
            .get_many::<String>("ignore")
            .unwrap()
            .map(|s| s.to_owned())
            .collect::<Vec<_>>();
        let ignore_tidy = args
            .get_many::<String>("ignore-tidy")
            .map(|val| val.map(|s| s.to_owned()).collect::<Vec<_>>());
        let ignore_format = args
            .get_many::<String>("ignore-format")
            .map(|val| val.map(|s| s.to_owned()).collect::<Vec<_>>());
        let extra_arg = convert_extra_arg_val(args);

        let lines_changed_only = LinesChangedOnly::from_string(
            args.get_one::<String>("lines-changed-only")
                .unwrap()
                .as_str(),
        );

        let thread_comments = ThreadComments::from_string(
            args.get_one::<String>("thread-comments").unwrap().as_str(),
        );

        let extensions = args
            .get_many::<String>("extensions")
            .unwrap()
            .filter_map(|s| {
                if s.is_empty() {
                    // filter out blank extensions here
                    None
                } else {
                    Some(s.to_string())
                }
            })
            .collect::<Vec<_>>();

        Self {
            version: args.get_one::<String>("version").unwrap().to_owned(),
            verbosity: args.get_one::<String>("verbosity").unwrap().as_str() == "debug",
            extensions,
            repo_root: args.get_one::<String>("repo-root").unwrap().to_owned(),
            lines_changed_only,
            files_changed_only: args.get_flag("files-changed-only"),
            ignore,
            style: args.get_one::<String>("style").unwrap().to_owned(),
            ignore_format,
            ignore_tidy,
            tidy_checks: args.get_one::<String>("tidy-checks").unwrap().to_owned(),
            database: args.get_one::<PathBuf>("database").map(|v| v.to_owned()),
            extra_arg,
            no_lgtm: args.get_flag("no-lgtm"),
            step_summary: args.get_flag("step-summary"),
            thread_comments,
            file_annotations: args.get_flag("file-annotations"),
            not_ignored: args
                .get_many::<String>("files")
                .map(|files| Vec::from_iter(files.map(|v| v.to_owned()))),
            tidy_review: args.get_flag("tidy-review"),
            format_review: args.get_flag("format-review"),
            passive_reviews: args.get_flag("passive-reviews"),
            delete_review_comments: args.get_flag("delete-review-comments"),
        }
    }
}

/// An enum to describe `--thread-comments` CLI option's behavior.
#[derive(PartialEq, Clone, Debug)]
pub enum ThreadComments {
    /// Always post a new comment and delete any outdated ones.
    On,
    /// Do not post thread comments.
    Off,
    /// Only update existing thread comments.
    /// If none exist, then post a new one.
    Update,
}

impl ThreadComments {
    fn from_string(val: &str) -> ThreadComments {
        match val {
            "true" | "on" | "1" => ThreadComments::On,
            "update" => ThreadComments::Update,
            _ => ThreadComments::Off,
        }
    }
}

impl Display for ThreadComments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThreadComments::On => write!(f, "true"),
            ThreadComments::Off => write!(f, "false"),
            ThreadComments::Update => write!(f, "update"),
        }
    }
}

/// A data structure to contain CLI options that relate to
/// clang-tidy or clang-format arguments.
#[derive(Debug, Clone, Default)]
pub struct ClangParams {
    pub tidy_checks: String,
    pub lines_changed_only: LinesChangedOnly,
    pub database: Option<PathBuf>,
    pub extra_args: Vec<String>,
    pub database_json: Option<Vec<CompilationUnit>>,
    pub style: String,
    pub clang_tidy_command: Option<PathBuf>,
    pub clang_format_command: Option<PathBuf>,
    pub tidy_filter: Option<FileFilter>,
    pub format_filter: Option<FileFilter>,
    pub tidy_review: bool,
    pub format_review: bool,
}

impl From<&Cli> for ClangParams {
    /// Construct a [`ClangParams`] instance from a [`Cli`] instance.
    fn from(args: &Cli) -> Self {
        ClangParams {
            tidy_checks: args.tidy_checks.clone(),
            lines_changed_only: args.lines_changed_only.clone(),
            database: args.database.clone(),
            extra_args: args.extra_arg.clone(),
            database_json: None,
            style: args.style.clone(),
            clang_tidy_command: None,
            clang_format_command: None,
            tidy_filter: args
                .ignore_tidy
                .as_ref()
                .map(|ignore_tidy| FileFilter::new(ignore_tidy, args.extensions.clone())),
            format_filter: args
                .ignore_format
                .as_ref()
                .map(|ignore_format| FileFilter::new(ignore_format, args.extensions.clone())),
            tidy_review: args.tidy_review,
            format_review: args.format_review,
        }
    }
}

/// A struct to contain CLI options that relate to
/// [`ResApiClient.post_feedback()`](fn@crate::rest_api::ResApiClient.post_feedback()).
pub struct FeedbackInput {
    pub thread_comments: ThreadComments,
    pub no_lgtm: bool,
    pub step_summary: bool,
    pub file_annotations: bool,
    pub style: String,
    pub tidy_review: bool,
    pub format_review: bool,
    pub passive_reviews: bool,
    pub delete_review_comments: bool,
}

impl From<&Cli> for FeedbackInput {
    /// Construct a [`FeedbackInput`] instance from a [`Cli`] instance.
    fn from(args: &Cli) -> Self {
        FeedbackInput {
            style: args.style.clone(),
            no_lgtm: args.no_lgtm,
            step_summary: args.step_summary,
            thread_comments: args.thread_comments.clone(),
            file_annotations: args.file_annotations,
            tidy_review: args.tidy_review,
            format_review: args.format_review,
            passive_reviews: args.passive_reviews,
            delete_review_comments: args.delete_review_comments,
        }
    }
}

impl Default for FeedbackInput {
    /// Construct a [`FeedbackInput`] instance with default values.
    fn default() -> Self {
        FeedbackInput {
            thread_comments: ThreadComments::Off,
            no_lgtm: true,
            step_summary: false,
            file_annotations: true,
            style: "llvm".to_string(),
            tidy_review: false,
            format_review: false,
            passive_reviews: false,
            delete_review_comments: false,
        }
    }
}

#[cfg(test)]
mod test {
    use crate::cli::get_arg_parser;

    use super::{Cli, LinesChangedOnly, ThreadComments};

    #[test]
    fn parse_positional() {
        let parser = get_arg_parser();
        let args = parser.get_matches_from(["cpp-linter", "file1.c", "file2.h"]);
        let cli = Cli::from(&args);
        let not_ignored = cli.not_ignored.expect("failed to parse positional args");
        assert!(!not_ignored.is_empty());
        assert!(not_ignored.contains(&String::from("file1.c")));
        assert!(not_ignored.contains(&String::from("file2.h")));
    }

    #[test]
    fn display_lines_changed_only_enum() {
        let input = "diff".to_string();
        assert_eq!(
            LinesChangedOnly::from_string(&input),
            LinesChangedOnly::Diff
        );
        assert_eq!(format!("{}", LinesChangedOnly::Diff), input);
    }

    #[test]
    fn display_thread_comments_enum() {
        let input = "false".to_string();
        assert_eq!(ThreadComments::from_string(&input), ThreadComments::Off);
        assert_eq!(format!("{}", ThreadComments::Off), input);
    }
}

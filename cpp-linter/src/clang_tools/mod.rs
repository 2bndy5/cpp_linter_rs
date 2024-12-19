//! This crate holds the functionality related to running clang-format and/or
//! clang-tidy.

use std::{
    env::current_dir,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Context, Result};
use git2::{DiffOptions, Patch};
// non-std crates
use lenient_semver;
use regex::Regex;
use semver::Version;
use tokio::task::JoinSet;
use which::{which, which_in};

// project-specific modules/crates
use super::common_fs::FileObj;
use crate::{
    cli::ClangParams,
    rest_api::{RestApiClient, COMMENT_MARKER, USER_OUTREACH},
};
pub mod clang_format;
use clang_format::run_clang_format;
pub mod clang_tidy;
use clang_tidy::{run_clang_tidy, CompilationUnit};

/// Fetch the path to a clang tool by `name` (ie `"clang-tidy"` or `"clang-format"`) and
/// `version`.
///
/// The specified `version` can be either
///
/// - a full or partial semantic version specification
/// - a path to a directory containing the executable binary `name`d
///
/// If the executable is not found using the specified `version`, then the tool is
/// sought only by it's `name`.
///
/// The only reason this function would return an error is if the specified tool is not
/// installed or present on the system (nor in the `$PATH` environment variable).
pub fn get_clang_tool_exe(name: &str, version: &str) -> Result<PathBuf> {
    if version.is_empty() {
        // The default CLI value is an empty string.
        // Thus, we should use whatever is installed and added to $PATH.
        if let Ok(cmd) = which(name) {
            return Ok(cmd);
        } else {
            return Err(anyhow!("Could not find clang tool by name"));
        }
    }
    if let Ok(semver) = lenient_semver::parse_into::<Version>(version) {
        // `version` specified has at least a major version number
        if let Ok(cmd) = which(format!("{}-{}", name, semver.major)) {
            Ok(cmd)
        } else if let Ok(cmd) = which(name) {
            // USERS SHOULD MAKE SURE THE PROPER VERSION IS INSTALLED BEFORE USING CPP-LINTER!!!
            // This block essentially ignores the version specified as a fail-safe.
            //
            // On Windows, the version's major number is typically not appended to the name of
            // the executable (or symlink for executable), so this is useful in that scenario.
            // On Unix systems, this block is not likely reached. Typically, installing clang
            // will produce a symlink to the executable with the major version appended to the
            // name.
            return Ok(cmd);
        } else {
            return Err(anyhow!("Could not find clang tool by name and version"));
        }
    } else {
        // `version` specified is not a semantic version; treat as path/to/bin
        if let Ok(exe_path) = which_in(name, Some(version), current_dir().unwrap()) {
            Ok(exe_path)
        } else {
            Err(anyhow!("Could not find clang tool by path"))
        }
    }
}

/// This creates a task to run clang-tidy and clang-format on a single file.
///
/// Returns a Future that infallibly resolves to a 2-tuple that contains
///
/// 1. The file's path.
/// 2. A collections of cached logs. A [`Vec`] of tuples that hold
///    - log level
///    - messages
fn analyze_single_file(
    file: Arc<Mutex<FileObj>>,
    clang_params: Arc<ClangParams>,
) -> Result<(PathBuf, Vec<(log::Level, String)>)> {
    let mut file = file
        .lock()
        .map_err(|_| anyhow!("Failed to lock file mutex"))?;
    let mut logs = vec![];
    if clang_params.clang_format_command.is_some() {
        if clang_params
            .format_filter
            .as_ref()
            .is_some_and(|f| f.is_source_or_ignored(file.name.as_path()))
            || clang_params.format_filter.is_none()
        {
            let format_result = run_clang_format(&mut file, &clang_params)?;
            logs.extend(format_result);
        } else {
            logs.push((
                log::Level::Info,
                format!(
                    "{} not scanned by clang-format due to `--ignore-format`",
                    file.name.as_os_str().to_string_lossy()
                ),
            ));
        }
    }
    if clang_params.clang_tidy_command.is_some() {
        if clang_params
            .tidy_filter
            .as_ref()
            .is_some_and(|f| f.is_source_or_ignored(file.name.as_path()))
            || clang_params.tidy_filter.is_none()
        {
            let tidy_result = run_clang_tidy(&mut file, &clang_params)?;
            logs.extend(tidy_result);
        } else {
            logs.push((
                log::Level::Info,
                format!(
                    "{} not scanned by clang-tidy due to `--ignore-tidy`",
                    file.name.as_os_str().to_string_lossy()
                ),
            ));
        }
    }
    Ok((file.name.clone(), logs))
}

/// A struct to contain the version numbers of the clang-tools used
#[derive(Default)]
pub struct ClangVersions {
    /// The clang-format version used.
    pub format_version: Option<String>,

    /// The clang-tidy version used.
    pub tidy_version: Option<String>,
}

/// Run `clang-tool --version`, then extract and return the version number.
fn capture_clang_version(clang_tool: &PathBuf) -> Result<String> {
    let output = Command::new(clang_tool).arg("--version").output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version_pattern = Regex::new(r"(?i)version\s*([\d.]+)").unwrap();
    let captures = version_pattern.captures(&stdout).ok_or(anyhow!(
        "Failed to find version number in `{} --version` output",
        clang_tool.to_string_lossy()
    ))?;
    Ok(captures.get(1).unwrap().as_str().to_string())
}

/// Runs clang-tidy and/or clang-format and returns the parsed output from each.
///
/// If `tidy_checks` is `"-*"` then clang-tidy is not executed.
/// If `style` is a blank string (`""`), then clang-format is not executed.
pub async fn capture_clang_tools_output(
    files: &mut Vec<Arc<Mutex<FileObj>>>,
    version: &str,
    clang_params: &mut ClangParams,
    rest_api_client: &impl RestApiClient,
) -> Result<ClangVersions> {
    let mut clang_versions = ClangVersions::default();
    // find the executable paths for clang-tidy and/or clang-format and show version
    // info as debugging output.
    if clang_params.tidy_checks != "-*" {
        let exe_path = get_clang_tool_exe("clang-tidy", version)?;
        let version_found = capture_clang_version(&exe_path)?;
        log::debug!(
            "{} --version: v{version_found}",
            &exe_path.to_string_lossy()
        );
        clang_versions.tidy_version = Some(version_found);
        clang_params.clang_tidy_command = Some(exe_path);
    }
    if !clang_params.style.is_empty() {
        let exe_path = get_clang_tool_exe("clang-format", version)?;
        let version_found = capture_clang_version(&exe_path)?;
        log::debug!(
            "{} --version: v{version_found}",
            &exe_path.to_string_lossy()
        );
        clang_versions.format_version = Some(version_found);
        clang_params.clang_format_command = Some(exe_path);
    }

    // parse database (if provided) to match filenames when parsing clang-tidy's stdout
    if let Some(db_path) = &clang_params.database {
        if let Ok(db_str) = fs::read(db_path.join("compile_commands.json")) {
            clang_params.database_json = Some(
                // A compilation database should be UTF-8 encoded, but file paths are not; use lossy conversion.
                serde_json::from_str::<Vec<CompilationUnit>>(&String::from_utf8_lossy(&db_str))
                    .with_context(|| "Failed to parse compile_commands.json")?,
            )
        }
    };

    let mut executors = JoinSet::new();
    // iterate over the discovered files and run the clang tools
    for file in files {
        let arc_params = Arc::new(clang_params.clone());
        let arc_file = Arc::clone(file);
        executors.spawn(async move { analyze_single_file(arc_file, arc_params) });
    }

    while let Some(output) = executors.join_next().await {
        if let Ok(out) = output? {
            let (file_name, logs) = out;
            rest_api_client.start_log_group(format!("Analyzing {}", file_name.to_string_lossy()));
            for (level, msg) in logs {
                log::log!(level, "{}", msg);
            }
            rest_api_client.end_log_group();
        }
    }
    Ok(clang_versions)
}

/// A struct to describe a single suggestion in a pull_request review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The file's line number in the diff that begins the suggestion.
    pub line_start: u32,
    /// The file's line number in the diff that ends the suggestion.
    pub line_end: u32,
    /// The actual suggestion.
    pub suggestion: String,
    /// The file that this suggestion pertains to.
    pub path: String,
}

/// A struct to describe the Pull Request review suggestions.
#[derive(Default)]
pub struct ReviewComments {
    /// The total count of suggestions from clang-tidy and clang-format.
    ///
    /// This differs from `comments.len()` because some suggestions may
    /// not fit within the file's diff.
    pub tool_total: [Option<u32>; 2],
    /// A list of comment suggestions to be posted.
    ///
    /// These suggestions are guaranteed to fit in the file's diff.
    pub comments: Vec<Suggestion>,
    /// The complete patch of changes to all files scanned.
    ///
    /// This includes changes from both clang-tidy and clang-format
    /// (assembled in that order).
    pub full_patch: [String; 2],
    /// The number of old review suggestions reused instead of posting anew.
    pub tool_reused: [u32; 2],
}

impl ReviewComments {
    pub fn summarize(&self, clang_versions: &ClangVersions) -> String {
        let mut body = format!("{COMMENT_MARKER}## Cpp-linter Review\n");
        for t in 0_usize..=1 {
            let mut total = 0;
            let (tool_name, tool_version) = if t == 0 {
                ("clang-format", clang_versions.format_version.as_ref())
            } else {
                ("clang-tidy", clang_versions.tidy_version.as_ref())
            };
            if tool_version.is_none() {
                // this tool was not used at all
                continue;
            }
            let tool_total = self.tool_total[t].unwrap_or_default();

            // If the tool's version is unknown, then we don't need to output this line.
            // NOTE: If the tool was invoked at all, then the tool's version shall be known.
            if let Some(ver_str) = tool_version {
                body.push_str(format!("\n### Used {tool_name} v{ver_str}\n").as_str());
            }
            for comment in &self.comments {
                if comment
                    .suggestion
                    .contains(format!("### {tool_name}").as_str())
                {
                    total += 1;
                }
            }

            if total != tool_total {
                body.push_str(
                    format!(
                        "\nOnly {total} out of {tool_total} _new_ {tool_name} concerns fit within this pull request's diff.\n",
                    )
                    .as_str(),
                );
            }
            if !self.full_patch[t].is_empty() {
                body.push_str(
                    format!(
                        "\n<details><summary>Click here for the full {tool_name} patch</summary>\n\n```diff\n{}```\n\n</details>\n",
                        self.full_patch[t]
                    ).as_str()
                );
            } else {
                body.push_str(
                    format!(
                        "\nNo concerns reported by {}. Great job! :tada:\n",
                        tool_name
                    )
                    .as_str(),
                )
            }
        }
        body.push_str(USER_OUTREACH);
        body
    }

    pub fn is_comment_in_suggestions(&mut self, comment: &Suggestion) -> bool {
        for s in &mut self.comments {
            if s.path == comment.path
                && s.line_end == comment.line_end
                && s.line_start == comment.line_start
            {
                s.suggestion.push('\n');
                s.suggestion.push_str(comment.suggestion.as_str());
                return true;
            }
        }
        false
    }

    /// Remove any reused ``Suggestion`` from the internal list and update counts
    pub fn remove_reused_suggestions(&mut self, existing_review_comments: Vec<Suggestion>) {
        if existing_review_comments.is_empty() {
            return;
        }
        let mut clang_tidy_comments = 0;
        let mut clang_format_comments = 0;
        self.comments = self
            .comments
            .iter()
            .filter(|s| {
                if !existing_review_comments.contains(s) {
                    clang_tidy_comments += s
                        .suggestion
                        .matches("### clang-tidy")
                        .collect::<Vec<&str>>()
                        .len() as u32;
                    clang_format_comments += s
                        .suggestion
                        .matches("### clang-format")
                        .collect::<Vec<&str>>()
                        .len() as u32;
                    return true;
                }
                false
            })
            .map(|s| s.to_owned())
            .collect();
        for (index, total_reused) in [clang_format_comments, clang_tidy_comments]
            .iter_mut()
            .enumerate()
        {
            if let Some(tool_total) = self.tool_total[index].as_mut() {
                if *total_reused > 0 {
                    self.tool_reused[index] = *tool_total - *total_reused;
                    *tool_total = *total_reused;
                }
            }
        }
    }
}

pub fn make_patch<'buffer>(
    path: &Path,
    patched: &'buffer [u8],
    original_content: &'buffer [u8],
) -> Result<Patch<'buffer>> {
    let mut diff_opts = &mut DiffOptions::new();
    diff_opts = diff_opts.indent_heuristic(true);
    diff_opts = diff_opts.context_lines(0);
    let patch = Patch::from_buffers(
        original_content,
        Some(path),
        patched,
        Some(path),
        Some(diff_opts),
    )
    .with_context(|| {
        format!(
            "Failed to create patch for file {}.",
            path.to_string_lossy()
        )
    })?;
    Ok(patch)
}

pub trait MakeSuggestions {
    /// Create some user-facing helpful info about what the suggestion aims to resolve.
    fn get_suggestion_help(&self, start_line: u32, end_line: u32) -> String;

    /// Get the tool's name which generated the advice.
    fn get_tool_name(&self) -> String;

    /// Create a bunch of suggestions from a [`FileObj`]'s advice's generated `patched` buffer.
    fn get_suggestions(
        &self,
        review_comments: &mut ReviewComments,
        file_obj: &FileObj,
        patch: &mut Patch,
        summary_only: bool,
    ) -> Result<()> {
        let is_tidy_tool = (&self.get_tool_name() == "clang-tidy") as usize;
        let hunks_total = patch.num_hunks();
        let mut hunks_in_patch = 0u32;
        let file_name = file_obj
            .name
            .to_string_lossy()
            .replace("\\", "/")
            .trim_start_matches("./")
            .to_owned();
        let patch_buf = &patch
            .to_buf()
            .with_context(|| "Failed to convert patch to byte array")?
            .to_vec();
        review_comments.full_patch[is_tidy_tool].push_str(
            String::from_utf8(patch_buf.to_owned())
                .with_context(|| format!("Failed to convert patch to string: {file_name}"))?
                .as_str(),
        );
        if summary_only {
            review_comments.tool_total[is_tidy_tool].get_or_insert(0);
            return Ok(());
        }
        for hunk_id in 0..hunks_total {
            let (hunk, line_count) = patch.hunk(hunk_id).with_context(|| {
                format!("Failed to get hunk {hunk_id} from patch for {file_name}")
            })?;
            hunks_in_patch += 1;
            let hunk_range = file_obj.is_hunk_in_diff(&hunk);
            if hunk_range.is_none() {
                continue;
            }
            let (start_line, end_line) = hunk_range.unwrap();
            let mut suggestion = String::new();
            let suggestion_help = self.get_suggestion_help(start_line, end_line);
            let mut removed = vec![];
            for line_index in 0..line_count {
                let diff_line = patch
                    .line_in_hunk(hunk_id, line_index)
                    .with_context(|| format!("Failed to get line {line_index} in a hunk {hunk_id} of patch for {file_name}"))?;
                let line = String::from_utf8(diff_line.content().to_owned())
                    .with_context(|| format!("Failed to convert line {line_index} buffer to string in hunk {hunk_id} of patch for {file_name}"))?;
                if ['+', ' '].contains(&diff_line.origin()) {
                    suggestion.push_str(line.as_str());
                } else {
                    removed.push(
                        diff_line
                            .old_lineno()
                            .expect("Removed line should have a line number"),
                    );
                }
            }
            if suggestion.is_empty() && !removed.is_empty() {
                suggestion.push_str(
                    format!(
                        "Please remove the line(s)\n- {}",
                        removed
                            .iter()
                            .map(|l| l.to_string())
                            .collect::<Vec<String>>()
                            .join("\n- ")
                    )
                    .as_str(),
                )
            } else {
                suggestion = format!("```suggestion\n{suggestion}```");
            }
            let comment = Suggestion {
                line_start: start_line,
                line_end: end_line,
                suggestion: format!("{suggestion_help}\n{suggestion}"),
                path: file_name.clone(),
            };
            if !review_comments.is_comment_in_suggestions(&comment) {
                review_comments.comments.push(comment);
            }
        }
        review_comments.tool_total[is_tidy_tool] =
            Some(review_comments.tool_total[is_tidy_tool].unwrap_or_default() + hunks_in_patch);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::get_clang_tool_exe;

    const TOOL_NAME: &str = "clang-format";

    #[test]
    fn get_exe_by_version() {
        let clang_version = env::var("CLANG_VERSION").unwrap_or("16".to_string());
        let tool_exe = get_clang_tool_exe(TOOL_NAME, clang_version.as_str());
        println!("tool_exe: {:?}", tool_exe);
        assert!(tool_exe.is_ok_and(|val| val
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string()
            .contains(TOOL_NAME)));
    }

    #[test]
    fn get_exe_by_default() {
        let tool_exe = get_clang_tool_exe(TOOL_NAME, "");
        println!("tool_exe: {:?}", tool_exe);
        assert!(tool_exe.is_ok_and(|val| val
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string()
            .contains(TOOL_NAME)));
    }

    use which::which;

    #[test]
    fn get_exe_by_path() {
        let clang_version = which(TOOL_NAME).unwrap();
        let bin_path = clang_version.parent().unwrap().to_str().unwrap();
        println!("binary exe path: {bin_path}");
        let tool_exe = get_clang_tool_exe(TOOL_NAME, bin_path);
        println!("tool_exe: {:?}", tool_exe);
        assert!(tool_exe.is_ok_and(|val| val
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string()
            .contains(TOOL_NAME)));
    }

    #[test]
    fn get_exe_by_invalid_path() {
        let tool_exe = get_clang_tool_exe(TOOL_NAME, "non-existent-path");
        assert!(tool_exe.is_err());
    }

    #[test]
    fn get_exe_by_invalid_name() {
        let clang_version = env::var("CLANG_VERSION").unwrap_or("16".to_string());
        let tool_exe = get_clang_tool_exe("not-a-clang-tool", &clang_version);
        assert!(tool_exe.is_err());
    }
}

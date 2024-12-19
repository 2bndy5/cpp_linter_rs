//! This module holds the Command Line Interface design.
use std::path::PathBuf;

// non-std crates
use clap::builder::{ArgPredicate, BoolishValueParser, FalseyValueParser};
use clap::{value_parser, Arg, ArgAction, ArgGroup, ArgMatches, Command};

mod structs;
pub use structs::{ClangParams, Cli, FeedbackInput, LinesChangedOnly, ThreadComments};

/// Builds and returns the Command Line Interface's argument parsing object.
pub fn get_arg_parser() -> Command {
    Command::new("cpp-linter")
        .subcommand(
            Command::new("version")
                .about("Display the cpp-linter version and exit.")
        )
        .arg(
            Arg::new("verbosity")
                .long("verbosity")
                .short('v')
                .default_value("info")
                .value_parser(["debug", "info"])
                .help(
                    "This controls the action's verbosity in the workflow's logs.
This option does not affect the verbosity of resulting
thread comments or file annotations.\n\n",
            ),
        )
        .arg(
            Arg::new("database")
                .long("database")
                .short('p')
                .value_parser(value_parser!(PathBuf))
                .help_heading("clang-tidy options")
                .help(
                    "The path that is used to read a compile command database.
For example, it can be a CMake build directory in which a file named
compile_commands.json exists (set `CMAKE_EXPORT_COMPILE_COMMANDS` to `ON`).
When no build path is specified, a search for compile_commands.json will be
attempted through all parent paths of the first input file. See [LLVM docs about
setup tooling](https://clang.llvm.org/docs/HowToSetupToolingForLLVM.html)
for an example of setting up Clang Tooling on a source tree.",
            )
        )
        .arg(
            Arg::new("style")
                .short('s')
                .long("style")
                .default_value("llvm")
                .help_heading("clang-format options")
                .help(
                    "The style rules to use.

- Set this to `file` to have clang-format use the closest relative
  .clang-format file.
- Set this to a blank string (`''`) to disable using clang-format
  entirely.

> [!NOTE]
> If this is not a blank string, then it is also passed to clang-tidy
> (if [`--tidy_checks`](#-c-tidy-checks) is not `-*`).
> This is done to ensure suggestions from both clang-tidy and
> clang-format are consistent.\n\n",
            ),
        )
        .arg(
            Arg::new("tidy-checks")
                .short('c')
                .long("tidy-checks")
                .default_value(
                    "boost-*,bugprone-*,performance-*,readability-*,portability-*,modernize-*,clang-analyzer-*,cppcoreguidelines-*",
                )
                .help_heading("clang-tidy options")
                .help(
                    "A comma-separated list of globs with optional `-` prefix.
Globs are processed in order of appearance in the list.
Globs without `-` prefix add checks with matching names to the set,
globs with the `-` prefix remove checks with matching names from the set of
enabled checks. This option's value is appended to the value of the 'Checks'
option in a .clang-tidy file (if any).

- It is possible to disable clang-tidy entirely by setting this option to
  `'-*'`.
- It is also possible to rely solely on a .clang-tidy config file by
  specifying this option as a blank string (`''`).

See also clang-tidy docs for more info.\n\n",
            ),
        )
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .default_missing_value("NO-VERSION")
                .num_args(0..=1)
                .require_equals(true)
                .default_value("")
                .help(
                    "The desired version of the clang tools to use. Accepted options are
strings which can be 8, 9, 10, 11, 12, 13, 14, 15, 16, 17.

- Set this option to a blank string (`''`) to use the
  platform's default installed version.
- This value can also be a path to where the clang tools are
  installed (if using a custom install location). All paths specified
  here are converted to absolute.\n\n",
            ),
        )
        .arg(
            Arg::new("extensions")
                .short('e')
                .long("extensions")
                .value_delimiter(',')
                .default_value("c,h,C,H,cpp,hpp,cc,hh,c++,h++,cxx,hxx")
                .help_heading("Source options")
                .help("A comma-separated list of file extensions to analyze.\n"),
        )
        .arg(
            Arg::new("repo-root")
                .short('r')
                .long("repo-root")
                .default_value(".")
                .help_heading("Source options")
                .help(
                    "The relative path to the repository root directory. This path is
relative to the runner's `GITHUB_WORKSPACE` environment variable (or
the current working directory if not using a CI runner).\n\n",
            ),
        )
        .arg(
            Arg::new("ignore")
                .short('i')
                .long("ignore")
                .value_delimiter('|')
                .default_value(".github|target")
                .help_heading("Source options")
                .help(
                    "Set this option with path(s) to ignore (or not ignore).

- In the case of multiple paths, you can use `|` to separate each path.
- There is no need to use `./` for each entry; a blank string (`''`)
  represents the repo-root path.
- This can also have files, but the file's path (relative to
  the [`--repo-root`](#-r-repo-root)) has to be specified with the filename.
- Submodules are automatically ignored. Hidden directories (beginning
  with a `.`) are also ignored automatically.
- Prefix a path with `!` to explicitly not ignore it. This can be
  applied to a submodule's path (if desired) but not hidden directories.
- Glob patterns are supported here. Path separators in glob patterns should
  use `/` because `\\` represents an escaped literal.\n\n",
            ),
        )
        .arg(
            Arg::new("ignore-tidy")
                .short('D')
                .long("ignore-tidy")
                .value_delimiter('|')
                .help_heading("clang-tidy options")
                .help(
                    "Similar to [`--ignore`](#-i-ignore) but applied
exclusively to files analyzed by clang-tidy.\n\n",
            ),
        )
        .arg(
            Arg::new("ignore-format")
                .short('M')
                .long("ignore-format")
                .value_delimiter('|')
                .help_heading("clang-format options")
                .help(
                    "Similar to [`--ignore`](#-i-ignore) but applied
exclusively to files analyzed by clang-format.\n\n",
            ),
        )
        .arg(
            Arg::new("lines-changed-only")
                .short('l')
                .long("lines-changed-only")
                .value_parser(["true", "on", "1", "false", "off", "0", "diff"])
                .default_value("true")
                .help_heading("Source options")
                .help(
                    "This controls what part of the files are analyzed.
The following values are accepted:

- `false`: All lines in a file are analyzed.
- `true`: Only lines in the diff that contain additions are analyzed.
- `diff`: All lines in the diff are analyzed (including unchanged
  lines but not subtractions).\n\n",
            ),
        )
        .arg(
            Arg::new("files-changed-only")
                .short('f')
                .long("files-changed-only")
                .default_value_if("lines-changed-only", ArgPredicate::Equals("true".into()), "true")
                .default_value("false")
                .value_parser(FalseyValueParser::new())
                .help_heading("Source options")
                .help(
                    "Set this option to false to analyze any source files in the repo.
This is automatically enabled if
[`--lines-changed-only`](#-l-lines-changed-only) is enabled.

> [!NOTE]
> The `GITHUB_TOKEN` should be supplied when running on a
> private repository with this option enabled, otherwise the runner
> does not not have the privilege to list the changed files for an event.
>
> See [Authenticating with the `GITHUB_TOKEN`](
> https://docs.github.com/en/actions/reference/authentication-in-a-workflow).\n\n",
            ),
        )
        .arg(
            Arg::new("extra-arg")
                .long("extra-arg")
                .short('x')
                .action(ArgAction::Append)
                .help_heading("clang-tidy options")
                .help(
                    r#"A string of extra arguments passed to clang-tidy for use as
compiler arguments. This can be specified more than once for each
additional argument. Recommend using quotes around the value and
avoid using spaces between name and value (use `=` instead):

```shell
cpp-linter --extra-arg="-std=c++17" --extra-arg="-Wall"
```"#,
            ),
        )
        .arg(
            Arg::new("thread-comments")
                .long("thread-comments")
                .short('g')
                .value_parser(["true", "on", "1", "false", "off", "0", "update"])
                .default_value("false")
                .help_heading("feedback options")
                .help(
                    "Set this option to true to enable the use of thread comments as feedback.
Set this to `update` to update an existing comment if one exists;
the value 'true' will always delete an old comment and post a new one if necessary.

> [!NOTE]
> To use thread comments, the `GITHUB_TOKEN` (provided by
> Github to each repository) must be declared as an environment
> variable.
>
> See [Authenticating with the `GITHUB_TOKEN`](
> https://docs.github.com/en/actions/reference/authentication-in-a-workflow).\n\n",
            ),
        )
        .arg(
            Arg::new("no-lgtm")
                .long("no-lgtm")
                .short('t')
                .value_parser(FalseyValueParser::new())
                .default_value("true")
                .help_heading("feedback options")
                .help(
                    "Set this option to true or false to enable or disable the use of a
thread comment that basically says 'Looks Good To Me' (when all checks pass).

> [!IMPORTANT]
> The [`--thread-comments`](#-g-thread-comments)
> option also notes further implications.\n\n",
            ),
        )
        .arg(
            Arg::new("step-summary")
                .long("step-summary")
                .short('w')
                .value_parser(FalseyValueParser::new())
                .default_value("false")
                .help_heading("feedback options")
                .help(
                    "Set this option to true or false to enable or disable the use of
a workflow step summary when the run has concluded.\n\n",
            ),
        )
        .arg(
            Arg::new("file-annotations")
                .long("file-annotations")
                .short('a')
                .value_parser(FalseyValueParser::new())
                .default_value("true")
                .help_heading("feedback options")
                .help(
                    "Set this option to false to disable the use of
file annotations as feedback.\n\n",
            ),
        )
        .arg(
            Arg::new("tidy-review")
                .long("tidy-review")
                .short('d')
                .value_parser(BoolishValueParser::new())
                .default_value("false")
                .help_heading("feedback options")
                .help(
                    "Set to `true` to enable Pull Request reviews from clang-tidy.\n\n",
            ),
        )
        .arg(
            Arg::new("format-review")
                .long("format-review")
                .short('m')
                .value_parser(BoolishValueParser::new())
                .default_value("false")
                .help_heading("feedback options")
                .help(
                    "Set to `true` to enable Pull Request reviews from clang-format.\n\n",
            ),
        )
        .arg(
            Arg::new("passive-reviews")
                .long("passive-reviews")
                .short('R')
                .value_parser(BoolishValueParser::new())
                .default_value("false")
                .help_heading("feedback options")
                .help(
                    "Set to `true` to prevent Pull Request reviews from
approving or requesting changes.\n\n",
            ),
        )
        .arg(
            Arg::new("delete-review-comments")
                .long("delete-review-comments")
                .short('E')
                .value_parser(BoolishValueParser::new())
                .default_value("true")
                .help(
                    "Set to `true` to delete outdated/resolved
suggestions in generated Pull Request reviews.

Enabling this may not be desirable for various reasons\n\n",
            ),
        )
        .arg(
            Arg::new("files")
                .action(ArgAction::Append)
                .help(
                    "An explicit path to a file.
This can be specified zero or more times, resulting in a list of files.
The list of files is appended to the internal list of 'not ignored' files.
Further filtering can still be applied (see [Source options](#source-options)).",
            )
        )
        .groups([
            ArgGroup::new("Clang-tidy options")
                .args(["tidy-checks", "database", "extra-arg", "ignore-tidy"])
                .multiple(true)
                .required(false),
            ArgGroup::new("Clang-format options")
                .args(["style", "ignore-format"])
                .multiple(true)
                .required(false),
            ArgGroup::new("General options")
                .args(["verbosity", "version"])
                .multiple(true)
                .required(false),
            ArgGroup::new("Source options")
                .args(["extensions", "repo-root", "ignore", "lines-changed-only", "files-changed-only"])
                .multiple(true)
                .required(false),
            ArgGroup::new("Feedback options")
                .args([
                    "thread-comments",
                    "no-lgtm",
                    "step-summary",
                    "file-annotations",
                    "tidy-review",
                    "format-review",
                    "passive-reviews",
                    "delete-review-comments",
                ])
                .multiple(true)
                .required(false),
        ])
        .next_line_help(true)
}

/// Converts the parsed value of the `--extra-arg` option into an optional vector of strings.
///
/// This is for adapting to 2 scenarios where `--extra-arg` is either
///
/// - specified multiple times
///     - each val is appended to a [`Vec`] (by clap crate)
/// - specified once with multiple space-separated values
///     - resulting [`Vec`] is made from splitting at the spaces between
/// - not specified at all (returns [`None`])
///
/// It is preferred that the values specified in either situation do not contain spaces and are
/// quoted:
/// ```shell
/// --extra-arg="-std=c++17" --extra-arg="-Wall"
/// # or equivalently
/// --extra-arg="-std=c++17 -Wall"
/// ```
/// The cpp-linter-action (for Github CI workflows) can only use 1 `extra-arg` input option, so
/// the value will be split at spaces.
pub fn convert_extra_arg_val(args: &ArgMatches) -> Vec<String> {
    let mut val = args.get_many::<String>("extra-arg").unwrap_or_default();
    if val.len() == 1 {
        // specified once; split and return result
        val.next()
            .unwrap()
            .trim_matches('\'')
            .trim_matches('"')
            .split(' ')
            .map(|i| i.to_string())
            .collect()
    } else {
        // specified multiple times; just return
        val.map(|i| i.to_string()).collect()
    }
}

#[cfg(test)]
mod test {
    use clap::ArgMatches;

    use super::{convert_extra_arg_val, get_arg_parser, Cli};

    fn parser_args(input: Vec<&str>) -> ArgMatches {
        let arg_parser = get_arg_parser();
        arg_parser.get_matches_from(input)
    }

    #[test]
    fn ignore_blank_extensions() {
        let args = parser_args(vec!["cpp-linter", "-e", "c,,h"]);
        let cli = Cli::from(&args);
        assert!(!cli.extensions.contains(&"".to_string()));
    }

    #[test]
    fn extra_arg_0() {
        let args = parser_args(vec!["cpp-linter"]);
        let extras = convert_extra_arg_val(&args);
        assert!(extras.is_empty());
    }

    #[test]
    fn extra_arg_1() {
        let args = parser_args(vec!["cpp-linter", "--extra-arg='-std=c++17 -Wall'"]);
        let extra_args = convert_extra_arg_val(&args);
        assert_eq!(extra_args.len(), 2);
        assert_eq!(extra_args, ["-std=c++17", "-Wall"])
    }

    #[test]
    fn extra_arg_2() {
        let args = parser_args(vec![
            "cpp-linter",
            "--extra-arg=-std=c++17",
            "--extra-arg=-Wall",
        ]);
        let extra_args = convert_extra_arg_val(&args);
        assert_eq!(extra_args.len(), 2);
        assert_eq!(extra_args, ["-std=c++17", "-Wall"])
    }
}

use crate::common_fs::{FileFilter, FileObj};
pub fn parse_diff(diff: &git2::Diff, file_filter: &FileFilter) -> Vec<FileObj> {
            && file_filter.is_source_or_ignored(&file_path)
pub fn parse_diff_from_buf(buff: &[u8], file_filter: &FileFilter) -> Vec<FileObj> {
        parse_diff(diff_obj, file_filter)
        brute_force_parse_diff::parse_diff(&String::from_utf8_lossy(buff), file_filter)
    use crate::common_fs::{FileFilter, FileObj};
        if !diff_binary_file.is_match(front_matter) {
    pub fn parse_diff(diff: &str, file_filter: &FileFilter) -> Vec<FileObj> {
            let hunk_start = if let Some(first_hunk) = hunk_info.find(file_diff) {
                first_hunk.start()
                file_diff.len()
            };
            let front_matter = &file_diff[..hunk_start];
            if let Some(file_name) = get_filename_from_front_matter(front_matter) {
                let file_path = PathBuf::from(file_name);
                if file_filter.is_source_or_ignored(&file_path) {
                    let (added_lines, diff_chunks) = parse_patch(&file_diff[hunk_start..]);
                    results.push(FileObj::from(file_path, added_lines, diff_chunks));
                }
            // } else {
            //     // file has no changed content. moving on
            //     continue;
            // }
        use crate::{
            common_fs::{FileFilter, FileObj},
            git::parse_diff_from_buf,
        };
        static RENAMED_DIFF: &str = r#"diff --git a/tests/demo/some source.cpp b/tests/demo/some source.cpp
rename to /tests/demo/some source.cpp
diff --git a/some picture.png b/some picture.png
new file mode 100644
Binary files /dev/null and b/some picture.png differ
"#;
            let files = parse_diff_from_buf(
                diff_buf,
                &FileFilter::new(&["target"], vec![String::from("cpp")]),
            );
            assert!(!files.is_empty());
            assert!(files
                .first()
                .unwrap()
                .name
                .ends_with("tests/demo/some source.cpp"));
            let files = parse_diff_from_buf(
                diff_buf,
                &FileFilter::new(&["target"], vec![String::from("cpp")]),
            );
        fn setup_parsed(buf: &str, extensions: &[String]) -> (Vec<FileObj>, Vec<FileObj>) {
                parse_diff_from_buf(
                    buf.as_bytes(),
                    &FileFilter::new(&["target"], extensions.to_owned()),
                ),
                parse_diff(buf, &FileFilter::new(&["target"], extensions.to_owned())),
            let (files_from_buf, files_from_str) = setup_parsed(diff_buf, &[String::from("cpp")]);
            let (files_from_buf, files_from_str) = setup_parsed(diff_buf, &[String::from("png")]);
    use crate::{common_fs::FileFilter, github_api::GithubApiClient, rest_api::RestApiClient};
    async fn checkout_cpp_linter_py_repo(
        extensions: &[String],
        let file_filter = FileFilter::new(&["target"], extensions.to_owned());
        rest_api_client
            .get_list_of_changed_files(&file_filter)
            .await
    #[tokio::test]
    async fn with_no_changed_sources() {
        let extensions = vec!["cpp".to_string(), "hpp".to_string()];
        let files = checkout_cpp_linter_py_repo(sha, &extensions, &tmp, None).await;
    #[tokio::test]
    async fn with_changed_sources() {
        let extensions = vec!["cpp".to_string(), "hpp".to_string()];
        let files = checkout_cpp_linter_py_repo(sha, &extensions.clone(), &tmp, None).await;
            assert!(
                extensions.contains(&file.name.extension().unwrap().to_string_lossy().to_string())
            );
    #[tokio::test]
    async fn with_staged_changed_sources() {
        let extensions = vec!["cpp".to_string(), "hpp".to_string()];
            &extensions.clone(),
        )
        .await;
            assert!(
                extensions.contains(&file.name.extension().unwrap().to_string_lossy().to_string())
            );
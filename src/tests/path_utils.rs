use crate::utils::{join_absolute, normalize_path, path_to_string_list};

#[test]
fn test_root_path_empty() {
    let out = normalize_path("/");
    assert_eq!(out, "/");
}

#[test]
fn test_simple_path() {
    let out = normalize_path("/a/b/c");
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_trailing_slash() {
    let out = normalize_path("/a/b/c/");
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_double_slashes() {
    let out = normalize_path("///a//b///c");
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_current_dir() {
    let out = normalize_path("/a/./b/./c");
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_parent_dir_simple() {
    let out = normalize_path("/a/b/../c");
    assert_eq!(out, "/a/c");
}

#[test]
fn test_parent_dir_multiple() {
    let out = normalize_path("/a/b/c/../../d");
    assert_eq!(out, "/a/d");
}

#[test]
fn test_parent_dir_to_root() {
    let out = normalize_path("/a/../../..");
    assert_eq!(out, "/");
}

#[test]
fn test_relative_path_becomes_absolute() {
    let out = normalize_path("a/b/c");
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_only_slashes() {
    let out = normalize_path("/////");
    assert_eq!(out, "/");
}

#[test]
fn test_path_to_string_list_basic() {
    let parts = path_to_string_list("/a/b/c");
    assert_eq!(parts, ["a", "b", "c"]);
}

#[test]
fn test_path_to_string_list_with_dotdot() {
    let parts = path_to_string_list("/a/../b");
    assert_eq!(parts, ["a", "..", "b"]);
}

#[test]
fn test_join_absolute_basic() {
    let out = join_absolute(
        &["a", "b", "c"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    );
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_join_absolute_empty() {
    let out = join_absolute(&[]);
    assert_eq!(out, "/");
}

#[test]
fn test_mixed_weird_input() {
    let out = normalize_path("///a//./b/../c///");
    assert_eq!(out, "/a/c");
}

use crate::utils::{join_absolute, normalize_path, path_to_string_list};

#[test]
fn test_simple_path() {
    let out = normalize_path("/");
    assert_eq!(out, "/");

    let out = normalize_path("/a/b/c");
    assert_eq!(out, "/a/b/c");

    let out = normalize_path("/a/b/c/");
    assert_eq!(out, "/a/b/c");

    let out = join_absolute(
        &["a", "b", "c"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    );
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_double_slashes_and_resolution() {
    let out = normalize_path("///a//b///c");
    assert_eq!(out, "/a/b/c");

    let out = normalize_path("/a/./b/./c");
    assert_eq!(out, "/a/b/c");

    let out = normalize_path("/a/b/../c");
    assert_eq!(out, "/a/c");

    let out = normalize_path("/a/b/c/../../d");
    assert_eq!(out, "/a/d");

    let out = normalize_path("/a/../../..");
    assert_eq!(out, "/");

    let parts = path_to_string_list("/a/../b");
    assert_eq!(parts, ["a", "..", "b"]);

    let out = normalize_path("/////");
    assert_eq!(out, "/");

    let out = join_absolute(&[]);
    assert_eq!(out, "/");

    let out = normalize_path("///a//./b/../c///");
    assert_eq!(out, "/a/c");
}

#[test]
fn test_relative_path_becomes_absolute() {
    let out = normalize_path("a/b/c");
    assert_eq!(out, "/a/b/c");
}

#[test]
fn test_path_to_string_list_basic() {
    let parts = path_to_string_list("/a/b/c");
    assert_eq!(parts, ["a", "b", "c"]);
}

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn utc_now_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

pub fn u64_to_utc_datetime(timestamp: u64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp as i64, 0).expect("Invalid timestamp")
}

pub fn normalize_path<P: Into<PathBuf>>(path: P) -> String {
    join_absolute(&path_to_string_list(path))
}

pub fn path_to_string_list<P: Into<PathBuf>>(path: P) -> Vec<String> {
    let path: PathBuf = path.into();
    path.components()
        .filter_map(|c| match c {
            std::path::Component::RootDir => None,
            std::path::Component::CurDir => None,
            std::path::Component::ParentDir => Some("..".to_string()),
            std::path::Component::Normal(s) => {
                let s = s.to_string_lossy();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            _ => None,
        })
        .collect()
}

pub fn normalize_components(parts: &[String]) -> Vec<String> {
    let mut stack = vec![];
    for part in parts {
        match part.as_str() {
            "." => {}
            ".." => {
                stack.pop();
            }
            p => stack.push(p.to_string()),
        }
    }
    stack
}

pub fn join_absolute(ss: &[String]) -> String {
    let normalized = normalize_components(ss);
    format!("/{}", normalized.join("/"))
}

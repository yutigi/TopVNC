use super::*;

#[test]
fn passes_the_token_by_file_never_inline() {
    assert_eq!(
        cloudflared_args(Path::new("tunnel-token.txt")),
        [
            "tunnel",
            "--no-autoupdate",
            "run",
            "--token-file",
            "tunnel-token.txt"
        ]
        .map(OsString::from)
    );
}

#[test]
fn rejects_a_missing_or_directory_token_path() {
    let dir = std::env::temp_dir();
    let missing = dir.join(format!("topvnc-spike-no-token-{}", std::process::id()));
    assert!(check_token_file(&missing).is_err());
    assert!(check_token_file(&dir).is_err());
}

#[test]
fn accepts_an_existing_token_file() {
    let path = std::env::temp_dir().join(format!("topvnc-spike-token-{}", std::process::id()));
    std::fs::write(&path, "not a real token").unwrap();
    let result = check_token_file(&path);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(result, Ok(()));
}

use std::fs;

use car_go_clean::logging::{Logger, LoggerOptions};

#[test]
fn logger_writes_structured_json_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("car-go-clean.log");
    let logger = Logger::with_options(
        &path,
        LoggerOptions {
            max_bytes: 1024,
            max_files: 2,
        },
    )
    .unwrap();

    logger.info("daemon starting");

    let body = fs::read_to_string(&path).unwrap();
    let line = body.lines().next().unwrap();
    let event: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(event["level"], "INFO");
    assert_eq!(event["message"], "daemon starting");
    assert!(event["ts"].as_u64().is_some());
}

#[test]
fn logger_rotates_when_current_file_exceeds_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("car-go-clean.log");
    let logger = Logger::with_options(
        &path,
        LoggerOptions {
            max_bytes: 90,
            max_files: 2,
        },
    )
    .unwrap();

    logger.info("first long log line that should fill the file");
    logger.info("second long log line that should trigger rotation");

    assert!(path.exists());
    assert!(path.with_extension("log.1").exists());
    assert!(fs::read_to_string(path)
        .unwrap()
        .contains("second long log line"));
}

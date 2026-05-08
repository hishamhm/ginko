#[cfg(test)]
mod test {
    use std::path::PathBuf;

    #[test]
    fn works() {
        let mut cmd = snapbox::cmd::Command::new(snapbox::cmd::cargo_bin!("ginko"));

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut file_name = path.clone();
        file_name.push("tests/simple.dts");

        cmd = cmd.arg(file_name);

        let res = cmd.output().unwrap();
        assert_eq!(res.status.code(), Some(0));
        assert_eq!(
            &String::from_utf8_lossy(&res.stdout),
            "OK; No issues found\n"
        );
    }

    #[test]
    fn works_with_error() {
        let tmpdir = tempfile::tempdir().unwrap();

        let file_name = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/simple_with_error");
        let temp_name = tmpdir.path().join("file.dts");

        std::fs::copy(file_name, &temp_name).unwrap();

        let mut cmd = snapbox::cmd::Command::new(snapbox::cmd::cargo_bin!("ginko"));
        cmd = cmd.arg(temp_name);

        let res = cmd.output().unwrap();
        assert_eq!(res.status.code(), Some(1));
        assert!(&String::from_utf8_lossy(&res.stdout).contains("error = &missing_label;"));
    }
}

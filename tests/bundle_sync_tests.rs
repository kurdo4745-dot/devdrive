use anyhow::Result;
use devdrive::sync::{
    list_remote_projects, read_bundle_manifest, restore_project_bundle, sync_project_bundle,
};
use std::fs;
use tempfile::tempdir;

#[test]
fn test_bundle_sync_offline_end_to_end() -> Result<()> {
    let temp = tempdir()?;
    let local_project = temp.path().join("full-stack-app");
    let remote_backup = temp.path().join("RemoteDrive").join("full-stack-app");
    let restored_project = temp.path().join("restored-app");

    // 1. Create clean source code files
    fs::create_dir_all(local_project.join("src"))?;
    fs::create_dir_all(local_project.join("assets"))?;

    let main_rs_content = b"fn main() {\n    println!(\"Hello from DevDrive Bundle!\");\n}\n";
    let pkg_json_content = b"{\n  \"name\": \"devdrive-app\",\n  \"version\": \"1.0.0\"\n}\n";
    let logo_png_content = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRFAKE_PNG_HEADER_PAYLOAD";

    fs::write(local_project.join("src/main.rs"), main_rs_content)?;
    fs::write(local_project.join("package.json"), pkg_json_content)?;
    fs::write(local_project.join("assets/logo.png"), logo_png_content)?;

    // 2. Create junk / ephemeral items that MUST be filtered out
    fs::create_dir_all(local_project.join("node_modules").join("express"))?;
    fs::write(
        local_project.join("node_modules/express/index.js"),
        b"module.exports = {};",
    )?;
    fs::write(
        local_project.join("model.onnx"),
        b"BINARY_ONNX_HEAVY_WEIGHTS_123456",
    )?;
    fs::create_dir_all(local_project.join("target/release"))?;
    fs::write(
        local_project.join("target/release/app.dylib"),
        b"DYLIB_DATA",
    )?;
    fs::create_dir_all(local_project.join(".git/objects"))?;
    fs::write(local_project.join(".git/config"), b"[core]\n")?;

    // 3. Execute bundle sync (completely offline, tempdir)
    let sync_report = sync_project_bundle(&local_project, &remote_backup, false)?;

    assert_eq!(
        sync_report.files_packaged, 3,
        "Exactly 3 source files must be packaged"
    );
    assert!(sync_report.bundle_bytes > 0);
    assert!(sync_report.original_bytes > 0);

    // 4. In remote directory: ONLY bundle.tar.gz and manifest.json must exist (+ .devdrive-meta.json)
    let remote_entries: Vec<String> = fs::read_dir(&remote_backup)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let non_dot_entries: Vec<&String> = remote_entries.iter().filter(|n| !n.starts_with('.')).collect();
    assert_eq!(
        non_dot_entries.len(),
        2,
        "Remote project root must strictly contain 2 files (bundle.tar.gz, manifest.json), got: {:?}",
        non_dot_entries
    );
    assert!(remote_backup.join("bundle.tar.gz").exists());
    assert!(remote_backup.join("manifest.json").exists());
    assert!(remote_backup.join(".devdrive-meta.json").exists());

    // Loose source folders or files must NOT exist on remote
    assert!(!remote_backup.join("src").exists());
    assert!(!remote_backup.join("package.json").exists());
    assert!(!remote_backup.join("assets").exists());
    assert!(!remote_backup.join("node_modules").exists());
    assert!(!remote_backup.join("model.onnx").exists());

    // 5. Verify manifest.json contains ONLY clean source code and NO junk
    let manifest = read_bundle_manifest(&remote_backup).expect("manifest.json should be parseable");
    assert_eq!(manifest.total_files, 3);
    assert_eq!(manifest.entries.len(), 3);

    let paths: Vec<String> = manifest.entries.iter().map(|e| e.path.clone()).collect();
    assert!(paths.contains(&"src/main.rs".to_string()));
    assert!(paths.contains(&"package.json".to_string()));
    assert!(paths.contains(&"assets/logo.png".to_string()));

    for p in &paths {
        assert!(
            !p.contains("node_modules"),
            "Manifest must not contain node_modules: {}",
            p
        );
        assert!(!p.ends_with(".onnx"), "Manifest must not contain onnx: {}", p);
        assert!(!p.starts_with(".git"), "Manifest must not contain .git: {}", p);
        assert!(!p.starts_with("target"), "Manifest must not contain target: {}", p);
    }

    // 6. Restore project bundle to a completely separate directory
    let restore_report = restore_project_bundle(&remote_backup, &restored_project)?;
    assert_eq!(restore_report.files_restored, 3);

    // Verify all 3 files are accurately restored
    assert_eq!(
        fs::read(restored_project.join("src/main.rs"))?,
        main_rs_content
    );
    assert_eq!(
        fs::read(restored_project.join("package.json"))?,
        pkg_json_content
    );
    assert_eq!(
        fs::read(restored_project.join("assets/logo.png"))?,
        logo_png_content
    );

    // Verify ephemeral files are NOT present in restored destination
    assert!(!restored_project.join("node_modules").exists());
    assert!(!restored_project.join("model.onnx").exists());
    assert!(!restored_project.join("target").exists());

    // 7. Test listing remote projects discovers the bundle-synced project
    let remote_base = temp.path().join("RemoteDrive");
    let remote_list = list_remote_projects(&remote_base)?;
    assert_eq!(remote_list.len(), 1);
    assert_eq!(remote_list[0].name, "full-stack-app");
    assert_eq!(remote_list[0].file_count, 3);
    assert!(remote_list[0].total_bytes > 0);

    Ok(())
}

#[test]
fn test_bundle_restore_blocks_malicious_path_traversal() -> Result<()> {
    use std::io::Write;
    let temp = tempdir()?;
    let remote_backup = temp.path().join("remote_bundle");
    let restore_target = temp.path().join("safe_target");
    fs::create_dir_all(&remote_backup)?;
    fs::create_dir_all(&restore_target)?;

    // Create a raw malicious bundle archive attempting to write to ../../sensitive.txt
    let tar_gz_path = remote_backup.join("bundle.tar.gz");
    {
        let file = fs::File::create(&tar_gz_path)?;
        let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());

        let mut header = [0u8; 512];
        let name_bytes = b"../../sensitive.txt";
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");

        let payload = b"MALICIOUS_OVERWRITE";
        let size_str = format!("{:011o}\0", payload.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");

        header[148..156].fill(b' ');
        let sum: u32 = header.iter().map(|&b| b as u32).sum();
        let chksum_str = format!("{:06o}\0 ", sum);
        header[148..156].copy_from_slice(chksum_str.as_bytes());

        gz.write_all(&header)?;
        gz.write_all(payload)?;
        let padding = (512 - (payload.len() % 512)) % 512;
        if padding > 0 {
            gz.write_all(&vec![0u8; padding])?;
        }
        gz.write_all(&[0u8; 1024])?;
        gz.finish()?;
    }

    // Must return an error and fail
    let res = restore_project_bundle(&remote_backup, &restore_target);
    assert!(res.is_err(), "Path traversal entry must cause restore to fail");

    // Outside directory must not have sensitive.txt
    assert!(!temp.path().join("sensitive.txt").exists());

    Ok(())
}

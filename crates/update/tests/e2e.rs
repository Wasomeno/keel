//! End to end, offline: a throwaway signed `Keel.app` in a temp dir, served
//! from 127.0.0.1, signed with a test minisign key, staged and installed
//! over another throwaway bundle. Never touches ~/Applications or launchd,
//! and never spawns the relaunch waiter.
#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use std::process::Command;

use sha2::Digest;

fn sh(cmd: &mut Command) {
    let out = cmd.output().expect("spawn");
    assert!(out.status.success(), "{cmd:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.keel.dev</string>
  <key>CFBundleName</key><string>Keel</string>
  <key>CFBundleExecutable</key><string>keel</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
</dict></plist>"#
    )
}

/// A minimal, ad-hoc signed bundle with an internal symlink chain.
fn fake_app(parent: &Path, version: &str) -> PathBuf {
    let app = parent.join("Keel.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::create_dir_all(app.join("Contents/Resources/Versions/A")).unwrap();
    std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/keel")).unwrap();
    std::fs::write(app.join("Contents/Info.plist"), plist(version)).unwrap();
    std::fs::write(app.join("Contents/Resources/Versions/A/data.txt"), version).unwrap();
    std::os::unix::fs::symlink("Versions/A", app.join("Contents/Resources/Current")).unwrap();
    sh(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&app));
    app
}

/// Answers `count` requests on `listener` from `routes` (path, body).
fn serve(listener: TcpListener, routes: Vec<(String, Vec<u8>)>, count: usize) {
    std::thread::spawn(move || {
        for stream in listener.incoming().take(count) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                    break;
                }
            }
            match routes.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => {
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    let _ = stream.write_all(body);
                }
                None => write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap(),
            }
        }
    });
}

fn manifest(port: u16, version: &str, bytes: &[u8], signature: &str) -> Vec<u8> {
    let sha: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    serde_json::json!({
        "version": version,
        "notes_url": format!("https://github.com/Wasomeno/keel/releases/tag/v{version}"),
        "assets": { keel_update::arch_key(): {
            "url": format!("http://127.0.0.1:{port}/Keel-{version}-macos-arm64.tar.xz"),
            "sha256": sha, "size": bytes.len(), "signature": signature,
        }}
    })
    .to_string()
    .into_bytes()
}

#[test]
fn check_download_verify_stage_install() {
    let root = std::env::temp_dir().join(format!("kut-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("live")).unwrap();

    // The release: 0.9.0, tarred the way publish-mac.sh does.
    fake_app(&root.join("src"), "0.9.0");
    let archive = root.join("Keel-0.9.0-macos-arm64.tar.xz");
    sh(Command::new("/usr/bin/tar").args(["--no-xattrs", "-cJf"]).arg(&archive).arg("-C").arg(root.join("src")).arg("Keel.app"));
    let bytes = std::fs::read(&archive).unwrap();
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let pk = kp.pk.to_box().unwrap().to_string();
    let sig = minisign::sign(Some(&kp.pk), &kp.sk, std::io::Cursor::new(&bytes), None, None).unwrap().to_string();

    // A bad copy: one flipped byte, same manifest signature.
    let mut flipped = bytes.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x01;

    // Good path: manifest + archive from one loopback server.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = manifest(port, "0.9.0", &bytes, &sig);
    serve(listener, vec![
        ("/latest.json".into(), body),
        ("/Keel-0.9.0-macos-arm64.tar.xz".into(), bytes.clone()),
    ], 2);

    // SAFETY: this test binary has a single test touching the environment.
    unsafe { std::env::set_var("KEEL_UPDATE_URL", format!("http://127.0.0.1:{port}/latest.json")) };
    let (url, loopback) = keel_update::manifest_url().unwrap();
    assert!(loopback);
    let text = keel_update::fetch_manifest(&url).unwrap();
    let keel_update::Check::Update(offer) =
        keel_update::evaluate(&text, "0.2.3", &keel_update::arch_key(), loopback).unwrap()
    else {
        panic!("expected an update");
    };
    assert_eq!(offer.version, "0.9.0");

    let work = keel_update::new_work_dir().unwrap();
    let mut fractions = Vec::new();
    let path = keel_update::download(&offer.asset, &work, |f| fractions.push(f)).unwrap();
    assert!(fractions.last().copied().flatten().is_some_and(|f| f > 0.99), "{fractions:?}");
    keel_update::verify_signature(&path, &offer.asset.signature, &pk).unwrap();
    let ready = keel_update::stage(&path, &offer.version, offer.notes_url.clone()).unwrap();
    assert!(ready.app().join("Contents/Resources/Current").is_symlink(), "symlink survived");
    assert_eq!(keel_update::find_ready(&std::env::temp_dir(), "0.2.3").map(|r| r.version), Some("0.9.0".into()));

    // Install over a throwaway 0.2.3, sibling swap, no daemon.
    let live = fake_app(&root.join("live"), "0.2.3");
    std::fs::write(live.join("Contents/Resources/stale.txt"), "old").unwrap();
    let plist = root.join("no-such.plist");
    let installed = keel_update::install(
        &ready.app(),
        &live,
        Some(keel_update::Daemon { plist: &plist, label: "sh.keel.engine.test" }),
    )
    .unwrap();
    assert!(!installed.kickstarted);
    let prev = installed.previous.clone().expect("swap path");
    assert!(prev.join("Contents/Resources/stale.txt").exists());
    assert!(!live.join("Contents/Resources/stale.txt").exists());
    let info = std::fs::read_to_string(live.join("Contents/Info.plist")).unwrap();
    assert!(info.contains("<string>0.9.0</string>"));
    keel_update::codesign_ok(&live).unwrap();
    assert!(live.join("Contents/Resources/Current").is_symlink());

    // A flipped byte: download passes length, fails sha256; with a
    // matching manifest sha it still fails the signature.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let bad_port = listener.local_addr().unwrap().port();
    serve(listener, vec![("/Keel-0.9.0-macos-arm64.tar.xz".into(), flipped.clone())], 1);
    let mut asset = offer.asset.clone();
    asset.url = format!("http://127.0.0.1:{bad_port}/Keel-0.9.0-macos-arm64.tar.xz");
    let bad_dir = keel_update::new_work_dir().unwrap();
    let err = keel_update::download(&asset, &bad_dir, |_| {}).unwrap_err();
    assert_eq!(err.message, keel_update::msg::BAD_ARCHIVE);
    let bad_path = bad_dir.join("archive.tar.xz");
    std::fs::write(&bad_path, &flipped).unwrap();
    let err = keel_update::verify_signature(&bad_path, &sig, &pk).unwrap_err();
    assert_eq!(err.message, keel_update::msg::BAD_SIGNATURE);

    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&bad_dir);
    let _ = std::fs::remove_dir_all(&root);
}

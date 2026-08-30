//! E2e lane 3 (0.9.0): the at-rest encryption journey over the real binary —
//! encrypt, prove the bytes went dark, search and read prose throughout,
//! survive a restart, decrypt, prove the bytes are back. The sealing key
//! lives in the sandbox's file fallback (`ENGRAM_KEYRING=off` in the shared
//! harness), so nothing here touches a real keychain.

mod e2e_common;

use std::time::Duration;

use e2e_common::*;

/// The distinctive plaintext we grep the raw `.tepin` bytes for — sealed
/// storage must make it disappear; decryption must bring it back.
const MARKER: &str = "zanzibar swordfish protocol";

fn store_bytes(sb: &Sandbox) -> Vec<u8> {
    std::fs::read(sb.root.join("alpha/.engram/graph.tepin")).expect("store on disk")
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

fn enc_view(port: u16, id: &str) -> serde_json::Value {
    let raw = http_get(port, &format!("/projects/{id}/encryption")).expect("/encryption answers");
    serde_json::from_str(&raw).expect("encryption status is JSON")
}

fn wait_enc_state(port: u16, id: &str, want: &str) {
    assert!(
        eventually(Duration::from_secs(30), || {
            let v = enc_view(port, id);
            !v["graph"]["job"]["running"].as_bool().unwrap_or(false)
                && v["graph"]["state"].as_str() == Some(want)
        }),
        "graph store never reached state {want:?}: {}",
        enc_view(port, id)
    );
    let v = enc_view(port, id);
    assert!(
        v["graph"]["job"]["error"].is_null(),
        "migration reported an error: {v}"
    );
}

#[test]
fn encrypt_search_restart_decrypt_journey() {
    let sb = Sandbox::new("encjourney", 19320);
    let proj = sb.project("alpha");

    // A store with a distinctive plaintext body.
    let out = sb
        .cmd(&["serve", "--fake-embeddings"], &proj)
        .output()
        .unwrap();
    assert!(out.status.success(), "serve failed: {out:?}");
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    let id = project_id(port, "alpha").expect("alpha registered");
    let created = http_post(
        port,
        &format!("/projects/{id}/nodes"),
        &format!(
            r#"{{"type":"Decision","title":"adopt the {MARKER}","body":"the {MARKER} is our wire format","durability":"stable","source":"claude"}}"#
        ),
    )
    .expect("write lands");
    let node_id = serde_json::from_str::<serde_json::Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Plaintext at rest before the switch (stop releases the file cleanly).
    assert!(sb.cmd(&["stop"], &proj).output().unwrap().status.success());
    assert!(
        contains(&store_bytes(&sb), MARKER),
        "plaintext store carries the marker bytes"
    );

    // Flip the switch; the migration runs in the background — poll to done.
    assert!(
        sb.cmd(&["serve", "--fake-embeddings"], &proj)
            .output()
            .unwrap()
            .status
            .success()
    );
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    http_post(
        port,
        &format!("/projects/{id}/encryption"),
        r#"{"target":"graph","enabled":true}"#,
    )
    .expect("encryption toggle accepted");
    wait_enc_state(port, &id, "sealed");

    // Readers see prose: the node reads back, search answers without a
    // single ciphertext blob in the payload.
    let node = http_get(port, &format!("/projects/{id}/nodes/{node_id}")).expect("node reads");
    assert!(node.contains(MARKER), "sealed store serves prose: {node}");
    let search = http_get(
        port,
        &format!("/projects/{id}/search?q=zanzibar%20swordfish"),
    )
    .expect("search answers over the sealed store");
    assert!(
        !search.contains("enc1:"),
        "no ciphertext leaks into search: {search}"
    );

    // The bytes went dark — and the machine settings recorded the switch.
    assert!(sb.cmd(&["stop"], &proj).output().unwrap().status.success());
    let bytes = store_bytes(&sb);
    assert!(
        !contains(&bytes, MARKER),
        "sealed store must not carry plaintext marker bytes"
    );
    assert!(contains(&bytes, "enc1:"), "sealed blobs are on disk");
    let settings = std::fs::read_to_string(sb.home.join("settings.json")).unwrap();
    assert!(
        settings.contains("\"encrypt_graph\": true"),
        "the machine switch persisted: {settings}"
    );

    // Restart: the store self-describes, the key loads from the sandbox
    // fallback, prose is back without any migration.
    assert!(
        sb.cmd(&["serve", "--fake-embeddings"], &proj)
            .output()
            .unwrap()
            .status
            .success()
    );
    let port = sb.wait_core_healthy(CORE_HEALTH_WINDOW);
    let v = enc_view(port, &id);
    assert_eq!(v["graph"]["state"].as_str(), Some("sealed"), "{v}");
    let node = http_get(port, &format!("/projects/{id}/nodes/{node_id}")).expect("node reads");
    assert!(node.contains(MARKER), "restart reads prose: {node}");

    // Decrypt: state converges, the plaintext bytes return.
    http_post(
        port,
        &format!("/projects/{id}/encryption"),
        r#"{"target":"graph","enabled":false}"#,
    )
    .expect("decryption toggle accepted");
    wait_enc_state(port, &id, "plaintext");
    let node = http_get(port, &format!("/projects/{id}/nodes/{node_id}")).expect("node reads");
    assert!(node.contains(MARKER));
    assert!(sb.cmd(&["stop"], &proj).output().unwrap().status.success());
    assert!(
        contains(&store_bytes(&sb), MARKER),
        "decrypted store carries plaintext again"
    );
}

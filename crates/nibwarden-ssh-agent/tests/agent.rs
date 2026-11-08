//! Drives the agent over an in-memory stream with throwaway keys from `tests/keys`.

use nibwarden_ssh_agent::{Backend, Identity, Purpose, serve};
use signature::Verifier as _;
use ssh_key::{PublicKey, Signature, encoding::Decode as _};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use zeroize::Zeroizing;

const KEYS: [&str; 3] = ["ed25519", "rsa", "p384"];

fn key_file(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/keys/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

struct TestBackend {
    locked: bool,
}

#[async_trait::async_trait]
impl Backend for TestBackend {
    async fn identities(&self) -> Vec<Identity> {
        if self.locked {
            return Vec::new();
        }
        KEYS.iter()
            .map(|k| Identity::from_openssh(&key_file(&format!("{k}.pub")), *k).unwrap())
            .collect()
    }

    async fn private_key(&self, key_blob: &[u8], _: &Purpose) -> Option<Zeroizing<String>> {
        if self.locked {
            return None;
        }
        KEYS.iter()
            .find(|k| {
                Identity::from_openssh(&key_file(&format!("{k}.pub")), "")
                    .unwrap()
                    .blob
                    == key_blob
            })
            .map(|k| Zeroizing::new(key_file(k)))
    }
}

fn start(locked: bool) -> DuplexStream {
    let (client, server) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move { serve(server, &TestBackend { locked }).await.unwrap() });
    client
}

async fn call(stream: &mut DuplexStream, body: &[u8]) -> Vec<u8> {
    stream.write_u32(body.len() as u32).await.unwrap();
    stream.write_all(body).await.unwrap();
    let len = stream.read_u32().await.unwrap();
    let mut reply = vec![0; len as usize];
    stream.read_exact(&mut reply).await.unwrap();
    reply
}

fn string(s: &[u8]) -> Vec<u8> {
    let mut out = (s.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(s);
    out
}

fn sign_request(blob: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
    let mut msg = vec![13];
    msg.extend(string(blob));
    msg.extend(string(data));
    msg.extend(flags.to_be_bytes());
    msg
}

/// Parses IDENTITIES_ANSWER into (blob, comment) pairs.
fn identities(reply: &[u8]) -> Vec<(Vec<u8>, String)> {
    assert_eq!(reply[0], 12);
    let mut rest = &reply[5..];
    let mut take = || {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let s = rest[4..4 + len].to_vec();
        rest = &rest[4 + len..];
        s
    };
    let n = u32::from_be_bytes(reply[1..5].try_into().unwrap());
    (0..n)
        .map(|_| (take(), String::from_utf8(take()).unwrap()))
        .collect()
}

fn signature_of(reply: &[u8]) -> Signature {
    assert_eq!(reply[0], 14, "expected SIGN_RESPONSE, got {reply:?}");
    let mut blob = &reply[5..];
    Signature::decode(&mut blob).unwrap()
}

#[tokio::test]
async fn lists_and_signs_with_every_key_type() {
    let mut agent = start(false);
    let ids = identities(&call(&mut agent, &[11]).await);
    assert_eq!(
        ids.iter().map(|(_, c)| c.as_str()).collect::<Vec<_>>(),
        KEYS
    );

    let data = b"some session data";
    for (blob, name) in &ids {
        let public = PublicKey::from_bytes(blob).unwrap();
        let flags = if name == "rsa" { 4 } else { 0 };
        let signature = signature_of(&call(&mut agent, &sign_request(blob, data, flags)).await);
        public.key_data().verify(data, &signature).unwrap();
    }
}

#[tokio::test]
async fn rsa_flags_select_the_hash() {
    let mut agent = start(false);
    let rsa = Identity::from_openssh(&key_file("rsa.pub"), "").unwrap();
    for (flags, algorithm) in [(2, "rsa-sha2-256"), (4, "rsa-sha2-512"), (0, "ssh-rsa")] {
        let signature =
            signature_of(&call(&mut agent, &sign_request(&rsa.blob, b"x", flags)).await);
        assert_eq!(signature.algorithm().as_str(), algorithm);
    }
}

#[tokio::test]
async fn refuses_unknown_keys_writes_and_locked_vault() {
    let mut agent = start(false);
    assert_eq!(call(&mut agent, &sign_request(b"nope", b"x", 0)).await, [5]);
    // ADD_IDENTITY, REMOVE_ALL_IDENTITIES, LOCK, EXTENSION
    for kind in [17u8, 19, 22, 27] {
        assert_eq!(call(&mut agent, &[kind]).await, [5]);
    }
    // A malformed request gets FAILURE and the connection stays usable.
    assert_eq!(call(&mut agent, &[13, 0, 0, 0, 9]).await, [5]);
    assert_eq!(identities(&call(&mut agent, &[11]).await).len(), 3);

    let mut locked = start(true);
    assert!(identities(&call(&mut locked, &[11]).await).is_empty());
    let ed = Identity::from_openssh(&key_file("ed25519.pub"), "").unwrap();
    assert_eq!(
        call(&mut locked, &sign_request(&ed.blob, b"x", 0)).await,
        [5]
    );
}

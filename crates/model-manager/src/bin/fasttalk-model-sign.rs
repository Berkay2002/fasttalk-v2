use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().ok_or_else(usage)?;
    match command.as_str() {
        "public-key" => {
            let seed_path = arguments.next().ok_or_else(usage)?;
            let output = arguments.next().ok_or_else(usage)?;
            reject_extra(arguments)?;
            let signing_key = read_signing_key(Path::new(&seed_path))?;
            fs::write(
                &output,
                format!(
                    "{}\n",
                    STANDARD.encode(signing_key.verifying_key().to_bytes())
                ),
            )
            .map_err(|error| format!("write {output}: {error}"))?;
        }
        "sign" => {
            let seed_path = arguments.next().ok_or_else(usage)?;
            let manifest = arguments.next().ok_or_else(usage)?;
            let output = arguments.next().ok_or_else(usage)?;
            reject_extra(arguments)?;
            let signing_key = read_signing_key(Path::new(&seed_path))?;
            let bytes = fs::read(&manifest).map_err(|error| format!("read {manifest}: {error}"))?;
            fs::write(
                &output,
                format!("{}\n", STANDARD.encode(signing_key.sign(&bytes).to_bytes())),
            )
            .map_err(|error| format!("write {output}: {error}"))?;
        }
        "verify" => {
            let public_key = arguments.next().ok_or_else(usage)?;
            let manifest = arguments.next().ok_or_else(usage)?;
            let signature = arguments.next().ok_or_else(usage)?;
            reject_extra(arguments)?;
            verify_signature(
                &fs::read_to_string(&public_key)
                    .map_err(|error| format!("read {public_key}: {error}"))?,
                &fs::read(&manifest).map_err(|error| format!("read {manifest}: {error}"))?,
                &fs::read_to_string(&signature)
                    .map_err(|error| format!("read {signature}: {error}"))?,
            )?;
            println!("verified {manifest}");
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn verify_signature(public_key: &str, manifest: &[u8], signature: &str) -> Result<(), String> {
    let public_key: [u8; 32] = STANDARD
        .decode(public_key.trim())
        .map_err(|error| format!("decode public key: {error}"))?
        .try_into()
        .map_err(|_| "public key must contain exactly 32 bytes".to_owned())?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).map_err(|error| format!("public key: {error}"))?;
    let signature: [u8; 64] = STANDARD
        .decode(signature.trim())
        .map_err(|error| format!("decode signature: {error}"))?
        .try_into()
        .map_err(|_| "signature must contain exactly 64 bytes".to_owned())?;
    verifying_key
        .verify(manifest, &Signature::from_bytes(&signature))
        .map_err(|_| "manifest signature is not trusted".to_owned())
}

fn read_signing_key(path: &Path) -> Result<SigningKey, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "signing seed must contain exactly 32 raw bytes".to_owned())?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn reject_extra(mut arguments: impl Iterator<Item = String>) -> Result<(), String> {
    if arguments.next().is_some() {
        return Err(usage());
    }
    Ok(())
}

fn usage() -> String {
    "usage: fasttalk-model-sign public-key <seed> <output> | sign <seed> <manifest> <output> | verify <public-key> <manifest> <signature>".to_owned()
}

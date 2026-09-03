//! `CredentialRef` parsing is the door into the store from SQLite rows, so it gets
//! its own file: a malformed reference must be rejected loudly, never guessed.

use yukinal_credentials::{CredentialError, CredentialRef};

#[test]
fn parses_keychain_references() {
    let reference = CredentialRef::parse("keychain://ssh/ssh_deploy_key_1").expect("parse");
    assert_eq!(reference.service(), "ssh");
    assert_eq!(reference.account(), "ssh_deploy_key_1");
    assert_eq!(reference.to_string_ref(), "keychain://ssh/ssh_deploy_key_1");
}

#[test]
fn rejects_malformed_references() {
    for bad in [
        "ssh/key",            // no scheme
        "keychain://",        // nothing after scheme
        "keychain://ssh",     // no account
        "keychain:///key",    // empty service
        "keychain://ssh/",    // empty account
        "keychain://ssh/a/b", // nested slash
    ] {
        assert!(
            matches!(
                CredentialRef::parse(bad),
                Err(CredentialError::InvalidReference(_))
            ),
            "should reject {bad:?}",
        );
    }
}

#[test]
fn round_trip_via_wire_string() {
    let original = CredentialRef::new("openai", "default");
    let on_wire = original.to_string_ref();
    assert_eq!(CredentialRef::parse(&on_wire).expect("parse"), original);
}

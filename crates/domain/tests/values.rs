//! Value types accept what their network accepts, reject the rest, and keep
//! the bare-string JSON shape that stored events and intent hashes depend on.

use warden_domain::{
    Address, AssetCode, BchTxid, CashAddr, ConcordiumAccount, EmailAddress, IdempotencyKey,
    IntentId, NearAccount, SolAddress, SolSignature, StreamId, TimeZoneName, ToolCallId,
};

#[test]
fn sol_addresses_are_32_byte_base58() {
    let a: SolAddress = "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2"
        .parse()
        .unwrap();
    assert_eq!(
        a.to_string(),
        "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2"
    );
    assert_eq!(
        "11111111111111111111111111111111"
            .parse::<SolAddress>()
            .unwrap(),
        SolAddress::from_bytes([0; 32])
    );
    for bad in [
        "",
        "Acme111",
        "0OIl",
        "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2xx",
    ] {
        assert!(bad.parse::<SolAddress>().is_err(), "{bad}");
    }
}

#[test]
fn a_signature_is_not_an_address() {
    let sig = SolSignature::from_bytes([9; 64]).to_string();
    assert_eq!(
        sig.parse::<SolSignature>().unwrap(),
        SolSignature::from_bytes([9; 64])
    );
    assert!(sig.parse::<SolAddress>().is_err());
}

#[test]
fn json_shape_matches_a_plain_string() {
    let address = Address::SolPubkey(SolAddress::from_bytes([0; 32]));
    let json = serde_json::to_value(&address).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"type": "sol_pubkey", "value": "11111111111111111111111111111111"})
    );
    let bad = serde_json::json!({"type": "sol_pubkey", "value": "Acme111"});
    assert!(serde_json::from_value::<Address>(bad).is_err());
}

#[test]
fn near_accounts_follow_the_naming_rules() {
    for ok in ["alice.near", "a1", "app-1_x.testnet", &"f".repeat(64)] {
        assert!(ok.parse::<NearAccount>().is_ok(), "{ok}");
    }
    for bad in [
        "a",
        "Alice.near",
        ".alice",
        "alice.",
        "al..ice",
        &"f".repeat(65),
    ] {
        assert!(bad.parse::<NearAccount>().is_err(), "{bad}");
    }
}

#[test]
fn cashaddr_checks_the_checksum() {
    // From the CashAddr spec's examples.
    let ok = "bitcoincash:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6a";
    assert_eq!(ok.parse::<CashAddr>().unwrap().as_str(), ok);
    let bare = ok.trim_start_matches("bitcoincash:");
    assert_eq!(
        bare.parse::<CashAddr>().unwrap().as_str(),
        ok,
        "prefix defaults"
    );
    let upper = ok.to_uppercase();
    assert!(upper.parse::<CashAddr>().is_ok());
    let flipped = ok.replace("qpm2", "qpm3");
    assert!(flipped.parse::<CashAddr>().is_err(), "checksum");
    assert!(
        "bitcoincash:QPM2qsznhks23z7629mms6s4cwef74vcwvy22gdx6a"
            .parse::<CashAddr>()
            .is_err()
    );
}

#[test]
fn concordium_accounts_round_trip_with_their_checksum() {
    let account = ConcordiumAccount::from_bytes([3; 32]);
    let text = account.to_string();
    assert_eq!(text.parse::<ConcordiumAccount>().unwrap(), account);
    let mut broken = text.into_bytes();
    let last = broken.len() - 1;
    broken[last] = if broken[last] == b'2' { b'3' } else { b'2' };
    assert!(
        String::from_utf8(broken)
            .unwrap()
            .parse::<ConcordiumAccount>()
            .is_err()
    );
}

#[test]
fn hex_hashes_need_64_hex_chars() {
    let txid = "ab".repeat(32);
    assert_eq!(txid.parse::<BchTxid>().unwrap().to_string(), txid);
    assert!("ab".parse::<BchTxid>().is_err());
    assert!("zz".repeat(32).parse::<BchTxid>().is_err());
}

#[test]
fn text_values() {
    assert!("ops@acme.example".parse::<EmailAddress>().is_ok());
    for bad in ["ops", "@acme.example", "ops@acme", "o ps@acme.example"] {
        assert!(bad.parse::<EmailAddress>().is_err(), "{bad}");
    }
    assert!("SOL".parse::<AssetCode>().is_ok());
    assert!("".parse::<AssetCode>().is_err());
    assert!("S O L".parse::<AssetCode>().is_err());
    assert!("Europe/London".parse::<TimeZoneName>().is_ok());
    assert!("Mars/Olympus".parse::<TimeZoneName>().is_err());
    assert!("call_0_ab12".parse::<ToolCallId>().is_ok());
    assert!("".parse::<ToolCallId>().is_err());
}

#[test]
fn idempotency_keys_come_from_tool_calls() {
    let run = warden_domain::RunId::new(uuid::Uuid::nil());
    let call: ToolCallId = "call-1".parse().unwrap();
    let key = IdempotencyKey::for_tool_call(run, &call).unwrap();
    assert_eq!(key.as_str(), "00000000-0000-0000-0000-000000000000:call-1");
    assert!("has space".parse::<IdempotencyKey>().is_err());
}

#[test]
fn stream_ids_parse_back() {
    let id = IntentId::new(uuid::Uuid::from_u128(1));
    let stream = StreamId::Intent(id);
    assert_eq!(stream.to_string().parse::<StreamId>().unwrap(), stream);
    assert_eq!(stream.as_intent(), Some(id));
    assert!("intent:nope".parse::<StreamId>().is_err());
    assert!(
        "thing:00000000-0000-0000-0000-000000000001"
            .parse::<StreamId>()
            .is_err()
    );
}

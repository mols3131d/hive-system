use mysql::Conn;
use mysql::prelude::Queryable;

use super::{ADMIN, Fixture, assert_diagnostics, commit, id, storage_state};

fn insert_bot(admin: &mut Conn, bot_id: &str, slug: &str, display_name: &str, retired: bool) {
    admin
        .exec_drop(
            "INSERT INTO bots (id, slug, display_name, revision, retired_at)
             VALUES (?, ?, ?, 'opaque-bot-revision', IF(?, UTC_TIMESTAMP(6), NULL))",
            (bot_id, slug, display_name, retired),
        )
        .unwrap();
}

#[test]
#[ignore = "requires the pinned Dolt binary at HIVE_DOLT_BIN"]
fn validates_committed_queen_registry_and_redacts_malformed_values() {
    let fixture = Fixture::start();
    let mut admin = fixture.connect(ADMIN);
    // Multiple active and retired Queens are valid, and existing issue_claims do not bind bots.
    insert_bot(&mut admin, &id(0x90), "queen-alpha", "Queen Alpha", false);
    insert_bot(&mut admin, &id(0x91), "queen-2-beta", "Queen Beta", false);
    insert_bot(
        &mut admin,
        &id(0x92),
        "queen-retired",
        "Retired Queen",
        true,
    );
    commit(&mut admin);
    let valid = fixture.check(0, &mut admin);
    let baseline = storage_state(&mut admin);

    let (id93, id94, id95, id96) = (id(0x93), id(0x94), id(0x95), id(0x96));
    let uppercase_id = id(0x9a).to_ascii_uppercase();
    let uuidv4_id = id(0x9b).replace("-7000-", "-4000-");
    let invalid_id = "invalid-bot-id-fixture";

    let malformed = [
        (uuidv4_id.as_str(), "queen-v4-id", "Retired V4 Queen", true),
        (id93.as_str(), "Queen-Upper", "Upper Slug Queen", false),
        (id94.as_str(), "queen_under", "Retired Under Queen", true),
        (id95.as_str(), "queen--double", "Double Hyphen Queen", false),
        (id96.as_str(), "../queen", "Traversal Queen", false),
        (uppercase_id.as_str(), "queen-upper-id", "Upper ID", false),
        (invalid_id, "bad/slug", "Invalid Bot Row", true),
    ];
    for (bot_id, slug, display_name, retired) in malformed {
        insert_bot(&mut admin, bot_id, slug, display_name, retired);
    }

    let dirty = storage_state(&mut admin);
    assert_ne!(dirty.1, baseline.1);
    assert_eq!(dirty.2, baseline.2);
    assert_eq!(fixture.check(0, &mut admin), valid);

    admin.query_drop("CALL DOLT_ADD('bots')").unwrap();
    assert_eq!(storage_state(&mut admin).2, dirty.1);
    assert_eq!(fixture.check(0, &mut admin), valid);

    commit(&mut admin);
    let report = fixture.check(4, &mut admin);
    assert_diagnostics(
        &report,
        &[
            ("INVALID_ID", "bots", &[]),
            ("INVALID_BOT_SLUG", "bots", &[&id93]),
            ("INVALID_BOT_SLUG", "bots", &[&id94]),
            ("INVALID_BOT_SLUG", "bots", &[&id95]),
            ("INVALID_BOT_SLUG", "bots", &[&id96]),
            ("INVALID_ID", "bots", &[]),
            ("INVALID_ID", "bots", &[]),
            ("INVALID_BOT_SLUG", "bots", &[]),
        ],
    );
    let report_text = report.to_string();
    for sensitive in [
        invalid_id,
        &uppercase_id,
        &uuidv4_id,
        "Queen-Upper",
        "queen_under",
        "queen--double",
        "../queen",
        "bad/slug",
        "opaque-bot-revision",
        "Queen Alpha",
    ] {
        assert!(!report_text.contains(sensitive), "{sensitive}");
    }
    assert_eq!(fixture.check(4, &mut admin), report);

    for (bot_id, _, _, _) in malformed {
        admin
            .exec_drop("DELETE FROM bots WHERE id=?", (bot_id,))
            .unwrap();
    }
    commit(&mut admin);
    let restored = fixture.check(0, &mut admin);
    assert_eq!(restored["data"], valid["data"]);
}

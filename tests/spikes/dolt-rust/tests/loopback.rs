use hive_dolt_spike::{ConnectError, Target, begin};
use mysql::Transaction;
use mysql::prelude::Queryable;
use std::error::Error;
use std::io::ErrorKind;

const AREA: &str = "01900000-0000-7000-8000-000000000101";
const ISSUE: &str = "01900000-0000-7000-8000-000000000102";
const BASE_REVISION: &str = "01900000-0000-7000-8000-000000000103";
const CLAIM: &str = "01900000-0000-7000-8000-000000000104";
const REQUEST: &str = "01900000-0000-7000-8000-000000000105";
const CLAIM_REVISION: &str = "01900000-0000-7000-8000-000000000106";
const HOLDER: &str = "fixture-holder";
const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn head(connection: &mut impl Queryable) -> mysql::Result<String> {
    let hash: String = connection
        .query_first("SELECT DOLT_HASHOF('HEAD')")?
        .expect("expected a Dolt HEAD hash");
    assert!(
        hash.len() == 32
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'v').contains(&b))
    );
    Ok(hash)
}

fn write_claim(transaction: &mut Transaction<'_>, base: &str, result: &str) -> mysql::Result<()> {
    transaction.exec_drop(
        "UPDATE issues SET status = 'in-progress', revision = ?, updated_at = UTC_TIMESTAMP(6)
         WHERE id = ? AND status = 'todo' AND revision = ?",
        (CLAIM_REVISION, ISSUE, BASE_REVISION),
    )?;
    assert_eq!(
        transaction.affected_rows(),
        1,
        "expected to update the fixture issue"
    );
    transaction.exec_drop(
        "INSERT INTO issue_claims (issue_id, claim_id, holder_ref, acquired_at)
         VALUES (?, ?, ?, UTC_TIMESTAMP(6))",
        (ISSUE, CLAIM, HOLDER),
    )?;
    transaction.exec_drop(
        "INSERT INTO operation_receipts
         (request_id, kind, fingerprint, actor_ref, authorization_ref, workflow_evidence_json,
          base_commit, result_json, created_at)
         VALUES (?, 'claim', ?, ?, 'fixture-authorization', '{}', ?, ?, UTC_TIMESTAMP(6))",
        (REQUEST, FINGERPRINT, HOLDER, base, result),
    )
}

fn check_refusals(target: &Target) -> Result<(), Box<dyn Error>> {
    let cases = [
        (
            "wrong password",
            Target {
                password: "wrong-fixture-password".to_owned(),
                ..target.clone()
            },
            ConnectError::Server(1045),
        ),
        (
            "passwordless root",
            Target {
                user: "root".to_owned(),
                password: String::new(),
                ..target.clone()
            },
            ConnectError::Server(1045),
        ),
        (
            "wrong database",
            Target {
                database: "absent_fixture".to_owned(),
                ..target.clone()
            },
            // Dolt 2.7.6 reports database access denied during ComInitDB as 1105.
            ConnectError::Server(1105),
        ),
        (
            "wrong workspace ID",
            Target {
                workspace_id: "01900000-0000-7000-8000-000000000199".to_owned(),
                ..target.clone()
            },
            ConnectError::IdentityMismatch,
        ),
        (
            "unavailable endpoint",
            Target {
                port: std::env::var("HIVE_SPIKE_UNAVAILABLE_PORT")?.parse()?,
                ..target.clone()
            },
            ConnectError::Transport(ErrorKind::ConnectionRefused),
        ),
    ];
    for (case, rejected, expected) in cases {
        assert_eq!(rejected.connect().err(), Some(expected), "{case}");
    }
    Ok(())
}

#[test]
fn authenticated_loopback_and_versioned_claim() -> Result<(), Box<dyn Error>> {
    let target = Target::from_fixture_env()?;
    let mut writer = target.connect()?;
    let (version, user, writer_id): (String, String, u64) = writer
        .query_first("SELECT DOLT_VERSION(), CURRENT_USER(), CONNECTION_ID()")?
        .expect("expected server version and session identity");
    assert_eq!(version, "2.7.6");
    assert_eq!(user, format!("{}@127.0.0.1", target.user));
    println!(
        "PASS authenticated loopback session on Dolt {version} with verified database/workspace"
    );

    check_refusals(&target)?;
    println!(
        "PASS wrong password, passwordless root, wrong database/workspace, and unavailable port refused"
    );

    let mut seed = begin(&mut writer, true)?;
    seed.exec_drop(
        "INSERT INTO areas (id, slug, name, revision) VALUES (?, 'synthetic', 'Synthetic', ?)",
        (AREA, BASE_REVISION),
    )?;
    seed.exec_drop(
        "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
         VALUES (?, 1, ?, 'Synthetic', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
        (ISSUE, AREA, BASE_REVISION),
    )?;
    seed.commit()?;
    let base = head(&mut writer)?;
    let result =
        format!(r#"{{"issue_id":"{ISSUE}","claim_id":"{CLAIM}","revision":"{CLAIM_REVISION}"}}"#,);

    let mut rolled_back = begin(&mut writer, true)?;
    write_claim(&mut rolled_back, &base, &result)?;
    rolled_back.rollback()?;

    let mut reader = target.connect()?;
    assert_eq!(head(&mut reader)?, base, "rollback changed version HEAD");
    let issue: Option<(String, String)> =
        reader.exec_first("SELECT status, revision FROM issues WHERE id = ?", (ISSUE,))?;
    assert_eq!(issue, Some(("todo".to_owned(), BASE_REVISION.to_owned())));
    for query in [
        "SELECT COUNT(*) FROM issue_claims",
        "SELECT COUNT(*) FROM operation_receipts",
        "SELECT COUNT(*) FROM dolt_status",
    ] {
        assert_eq!(reader.query_first(query)?, Some(0u64), "{query}");
    }
    drop(reader);
    println!("PASS explicit rollback leaves the issue, claim, receipt, and clean HEAD at the base");

    let mut transaction = begin(&mut writer, true)?;
    write_claim(&mut transaction, &base, &result)?;
    transaction.commit()?;
    let confirmed = head(&mut writer)?;
    assert_ne!(confirmed, base, "SQL COMMIT did not publish a Dolt version");
    drop(writer);

    let mut reader = target.connect()?;
    assert_ne!(
        reader.query_first("SELECT CONNECTION_ID()")?,
        Some(writer_id)
    );
    assert_eq!(head(&mut reader)?, confirmed);
    assert_eq!(
        reader.query_first("SELECT COUNT(*) FROM dolt_status")?,
        Some(0u64)
    );
    let parents: Vec<String> = reader.exec(
        "SELECT parent_hash FROM dolt_commit_ancestors WHERE commit_hash = ?",
        (&confirmed,),
    )?;
    assert_eq!(
        parents.as_slice(),
        std::slice::from_ref(&base),
        "expected one direct successor commit"
    );

    // Dolt 2.7.6 requires a literal in AS OF. head() checks the hash alphabet.
    let issue: Option<(String, String)> = reader.exec_first(
        format!("SELECT status, revision FROM issues AS OF '{confirmed}' WHERE id = ?"),
        (ISSUE,),
    )?;
    assert_eq!(
        issue,
        Some(("in-progress".to_owned(), CLAIM_REVISION.to_owned()))
    );
    let claims: Vec<(String, String, String)> = reader.query(format!(
        "SELECT issue_id, claim_id, holder_ref FROM issue_claims AS OF '{confirmed}'"
    ))?;
    assert_eq!(
        claims,
        [(ISSUE.to_owned(), CLAIM.to_owned(), HOLDER.to_owned())]
    );
    let receipts: Vec<(String, String, String, String, String)> = reader.query(format!(
        "SELECT request_id, kind, fingerprint, base_commit, result_json
         FROM operation_receipts AS OF '{confirmed}'"
    ))?;
    assert_eq!(
        receipts,
        [(
            REQUEST.to_owned(),
            "claim".to_owned(),
            FINGERPRINT.to_owned(),
            base.clone(),
            result
        )]
    );
    for table in ["issue_claims", "operation_receipts"] {
        let rows: Option<u64> =
            reader.query_first(format!("SELECT COUNT(*) FROM {table} AS OF '{base}'"))?;
        assert_eq!(rows, Some(0), "{table} at base");
    }
    println!(
        "PASS independent reconnect read the claim and receipt in one native Dolt commit {base} -> {confirmed}"
    );
    Ok(())
}

#[test]
fn malformed_identity_is_refused() -> Result<(), Box<dyn Error>> {
    let target = Target::from_fixture_env()?;
    assert_eq!(target.connect().err(), Some(ConnectError::IdentityMismatch));
    println!("PASS malformed workspace identity returns a bounded typed refusal");
    Ok(())
}

#[test]
fn non_main_is_refused() -> Result<(), Box<dyn Error>> {
    let target = Target::from_fixture_env()?;
    let mut connection = mysql::Conn::new(
        mysql::OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(target.port)
            .prefer_socket(false)
            .db_name(Some(&target.database))
            .user(Some(&target.user))
            .pass(Some(&target.password)),
    )?;
    let identity: Option<(String, String, String)> = connection.query_first(
        "SELECT DATABASE(), ACTIVE_BRANCH(), workspace_id FROM workspace_meta WHERE singleton_id = 1",
    )?;
    assert_eq!(
        identity,
        Some((
            target.database.clone(),
            "fixture-other".to_owned(),
            target.workspace_id.clone(),
        ))
    );
    assert_eq!(target.connect().err(), Some(ConnectError::IdentityMismatch));
    println!("PASS authenticated non-main rejected for the expected database and workspace");
    Ok(())
}

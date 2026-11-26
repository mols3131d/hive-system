//! Synthetic process-loss tests driven by tests/spikes/dolt_recovery.py.

mod protocol;

use hive_dolt_spike::Target;
use mysql::Conn;
use mysql::prelude::Queryable;
use protocol::{
    ADVANCE, BASE_REVISION, CLAIM, CLAIM_ID, CLAIM_REVISION, Change, Confirmed, EDIT_REVISION,
    Fault, Fixture, HOLDER, ISSUE, OTHER_ISSUE, Outcome, PENDING, Pending, Receipt, Recovery,
    Request, Result, UNRELATED, clean, head, session_present,
};
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(15);

#[test]
fn death_before_commit() -> Result<()> {
    let (fixture, _witness) = Fixture::start()?;
    let base = fixture.seed()?;
    let mut worker = Worker::start(&fixture, "before")?;
    let pending = observe_pending(&fixture, &base)?;
    assert_base(&fixture, &base)?;

    worker.kill()?;
    await_session_end(&fixture, pending.session)?;
    let foreign_witness = Fixture::attach(Target::from_fixture_env()?, fixture.witness + 1)?;
    assert_eq!(
        foreign_witness.recover().unwrap_err().to_string(),
        "fixture server witness did not match"
    );
    assert!(fixture.pending()?.is_some());
    assert_eq!(fixture.recover()?, Recovery::RolledBack);
    assert!(fixture.pending()?.is_none());
    assert_base(&fixture, &base)?;

    let Outcome::Committed(retry) = fixture.submit(CLAIM)? else {
        panic!("the same request should succeed after proved rollback");
    };
    assert_eq!(retry, assert_claim(&fixture, &base, &retry.commit)?);
    assert!(fixture.pending()?.is_none());
    println!(
        "PASS pre-COMMIT SIGKILL: verified server-session teardown, unchanged base {base}, and one confirmed retry {}",
        retry.commit
    );
    Ok(())
}

#[test]
fn death_after_version_commit() -> Result<()> {
    let (fixture, _witness) = Fixture::start()?;
    let base = fixture.seed()?;
    let mut worker = Worker::start(&fixture, "after")?;
    let pending = observe_pending(&fixture, &base)?;
    worker.kill()?;
    await_session_end(&fixture, pending.session)?;
    let commit = head(&mut fixture.reader()?)?;
    let original = Outcome::Replayed(assert_claim(&fixture, &base, &commit)?);
    assert_eq!(fixture.submit(CLAIM)?, original);
    assert!(fixture.pending()?.is_none());

    let Outcome::Committed(advance) = fixture.submit(ADVANCE)? else {
        panic!("expected a separate versioned edit");
    };
    let mut reader = fixture.reader()?;
    let advanced = head(&mut reader)?;
    assert_eq!(advanced, advance.commit);
    assert_parent(&mut reader, &advanced, &commit)?;
    assert_issue(
        &mut reader,
        &advanced,
        ISSUE,
        "in-progress",
        EDIT_REVISION,
        "Revised synthetic issue",
    )?;

    assert_eq!(fixture.submit(CLAIM)?, original);
    let changed = Request {
        change: Change::Claim("different-synthetic-holder"),
        ..CLAIM
    };
    assert_eq!(fixture.submit(changed)?, Outcome::FingerprintConflict);
    let stale = Request {
        id: "01900000-0000-7000-8000-000000000290",
        ..CLAIM
    };
    assert_eq!(fixture.submit(stale)?, Outcome::StaleRevision);

    let mut reader = fixture.reader()?;
    assert_eq!(head(&mut reader)?, advanced);
    assert!(clean(&mut reader)?);
    assert_counts(&mut reader, &advanced, 1, 2)?;
    assert_issue(
        &mut reader,
        &advanced,
        ISSUE,
        "in-progress",
        EDIT_REVISION,
        "Revised synthetic issue",
    )?;
    assert!(fixture.pending()?.is_none());
    println!(
        "PASS post-COMMIT SIGKILL before caller outcome: one native claim and receipt at {commit}; replay preserved the original result after {advanced}; altered fingerprint and fresh stale request refused"
    );
    Ok(())
}

#[test]
fn working_set_receipt_is_unconfirmed() -> Result<()> {
    let (fixture, _witness) = Fixture::start()?;
    let base = fixture.seed()?;
    let mut worker = Worker::start(&fixture, "working")?;
    let pending = observe_pending(&fixture, &base)?;
    let marker = fs::read(PENDING)?;
    worker.kill()?;
    await_session_end(&fixture, pending.session)?;

    assert_eq!(fixture.submit(CLAIM)?, Outcome::CommitUncertain);
    assert_eq!(fixture.submit(UNRELATED)?, Outcome::CommitUncertain);
    assert_eq!(fs::read(PENDING)?, marker);
    let mut reader = fixture.reader()?;
    assert_eq!(head(&mut reader)?, base);
    assert!(!clean(&mut reader)?);
    assert_counts(&mut reader, &base, 0, 0)?;
    assert_issue(
        &mut reader,
        &base,
        ISSUE,
        "todo",
        BASE_REVISION,
        "Synthetic",
    )?;
    let working: Vec<(String, String, String)> =
        reader.query("SELECT request_id, base_commit, result_json FROM operation_receipts")?;
    assert_eq!(
        working,
        [(CLAIM.id.to_owned(), base.clone(), claim_result())]
    );
    let unrelated: Option<String> =
        reader.exec_first("SELECT revision FROM issues WHERE id = ?", (OTHER_ISSUE,))?;
    assert_eq!(unrelated.as_deref(), Some(BASE_REVISION));
    println!(
        "PASS version publication disabled: SQL receipt visible only in the working set; marker retained and both retries and unrelated writes blocked at {base}"
    );
    Ok(())
}

#[test]
fn live_session_blocks_unrelated_write() -> Result<()> {
    let (fixture, _witness) = Fixture::start()?;
    let base = fixture.seed()?;
    let mut worker = Worker::start(&fixture, "live")?;
    let pending = observe_pending(&fixture, &base)?;
    let marker = fs::read(PENDING)?;
    assert_base(&fixture, &base)?;

    assert_eq!(fixture.submit(UNRELATED)?, Outcome::CommitUncertain);
    assert_eq!(fs::read(PENDING)?, marker);
    assert_base(&fixture, &base)?;
    assert!(session_present(&mut fixture.reader()?, pending.session)?);

    worker.commit_and_exit()?;
    await_session_end(&fixture, pending.session)?;
    let commit = head(&mut fixture.reader()?)?;
    assert_claim(&fixture, &base, &commit)?;
    let Outcome::Committed(edit) = fixture.submit(UNRELATED)? else {
        panic!("the unrelated write should succeed after reconciliation");
    };
    let mut reader = fixture.reader()?;
    assert_eq!(head(&mut reader)?, edit.commit);
    assert_parent(&mut reader, &edit.commit, &commit)?;
    assert_counts(&mut reader, &edit.commit, 1, 2)?;
    assert_issue(
        &mut reader,
        &edit.commit,
        OTHER_ISSUE,
        "todo",
        EDIT_REVISION,
        "Revised synthetic issue",
    )?;
    assert!(fixture.pending()?.is_none());
    println!(
        "PASS clean base {base}: unrelated write blocked with guard free and prior SQL session live; that session published {commit}, then reconciliation admitted the unrelated write"
    );
    Ok(())
}

fn observe_pending(fixture: &Fixture, base: &str) -> Result<Pending> {
    let marker = fixture
        .pending()?
        .expect("worker must have persisted its marker");
    assert_eq!(marker.request, CLAIM.id);
    assert_eq!(marker.base, base);
    let mut observer = fixture.reader()?;
    assert_ne!(observer.connection_id(), marker.session);
    assert!(session_present(&mut observer, marker.session)?);
    Ok(marker)
}

fn await_session_end(fixture: &Fixture, session: u32) -> Result<()> {
    let mut observer = fixture.reader()?;
    until("recorded SQL session to end", || {
        Ok((!session_present(&mut observer, session)?).then_some(()))
    })
}

fn assert_base(fixture: &Fixture, base: &str) -> Result<()> {
    let mut reader = fixture.reader()?;
    assert_eq!(head(&mut reader)?, base);
    assert!(clean(&mut reader)?);
    assert_counts(&mut reader, base, 0, 0)?;
    for issue in [ISSUE, OTHER_ISSUE] {
        assert_issue(&mut reader, base, issue, "todo", BASE_REVISION, "Synthetic")?;
    }
    Ok(())
}

fn claim_result() -> String {
    format!(r#"{{"issue_id":"{ISSUE}","claim_id":"{CLAIM_ID}","revision":"{CLAIM_REVISION}"}}"#)
}

fn assert_claim(fixture: &Fixture, base: &str, commit: &str) -> Result<Confirmed> {
    // Verify the native snapshot independently of the protocol's history lookup.
    let mut reader = fixture.reader()?;
    assert_eq!(head(&mut reader)?, commit);
    assert!(clean(&mut reader)?);
    assert_parent(&mut reader, commit, base)?;
    assert_counts(&mut reader, base, 0, 0)?;
    assert_issue(&mut reader, base, ISSUE, "todo", BASE_REVISION, "Synthetic")?;
    assert_issue(
        &mut reader,
        commit,
        ISSUE,
        "in-progress",
        CLAIM_REVISION,
        "Synthetic",
    )?;
    assert_issue(
        &mut reader,
        commit,
        OTHER_ISSUE,
        "todo",
        BASE_REVISION,
        "Synthetic",
    )?;
    let claims: Vec<(String, String, String)> = reader.query(format!(
        "SELECT issue_id, claim_id, holder_ref FROM issue_claims AS OF '{commit}'"
    ))?;
    assert_eq!(
        claims,
        [(ISSUE.to_owned(), CLAIM_ID.to_owned(), HOLDER.to_owned())]
    );
    let receipts: Vec<(String, String, String, String, String, String)> = reader.query(format!(
        "SELECT request_id, kind, fingerprint, actor_ref, base_commit, result_json
         FROM operation_receipts AS OF '{commit}'"
    ))?;
    let [(id, kind, fingerprint, actor, original_base, result)] = receipts.as_slice() else {
        panic!("expected exactly one receipt in the claim's native commit");
    };
    assert_eq!(id, CLAIM.id);
    assert_eq!(kind, "claim");
    assert_eq!(actor, "synthetic-actor");
    assert_eq!(original_base, base);
    assert_eq!(result, &claim_result());
    Ok(Confirmed {
        commit: commit.to_owned(),
        receipt: Receipt {
            kind: kind.clone(),
            fingerprint: fingerprint.clone(),
            actor: actor.clone(),
            base: original_base.clone(),
            result: result.clone(),
        },
    })
}

fn assert_parent(reader: &mut Conn, commit: &str, parent: &str) -> Result<()> {
    let parents: Vec<String> = reader.exec(
        "SELECT parent_hash FROM dolt_commit_ancestors WHERE commit_hash = ?",
        (commit,),
    )?;
    assert_eq!(parents, [parent]);
    Ok(())
}

fn assert_counts(reader: &mut Conn, commit: &str, claims: u64, receipts: u64) -> Result<()> {
    for (table, expected) in [("issue_claims", claims), ("operation_receipts", receipts)] {
        let count: Option<u64> =
            reader.query_first(format!("SELECT COUNT(*) FROM {table} AS OF '{commit}'"))?;
        assert_eq!(count, Some(expected), "{table} at {commit}");
    }
    Ok(())
}

fn assert_issue(
    reader: &mut Conn,
    commit: &str,
    issue: &str,
    status: &str,
    revision: &str,
    title: &str,
) -> Result<()> {
    let row: Option<(String, String, String)> = reader.exec_first(
        format!("SELECT status, revision, title FROM issues AS OF '{commit}' WHERE id = ?"),
        (issue,),
    )?;
    assert_eq!(
        row,
        Some((status.to_owned(), revision.to_owned(), title.to_owned()))
    );
    Ok(())
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = until("killed worker to exit", || Ok(self.0.try_wait()?));
        }
    }
}

struct Worker {
    child: OwnedChild,
    control: UnixStream,
}

impl Worker {
    fn start(fixture: &Fixture, mode: &str) -> Result<Self> {
        let (mut control, worker_end) = UnixStream::pair()?;
        control.set_read_timeout(Some(TIMEOUT))?;
        control.set_write_timeout(Some(TIMEOUT))?;
        let child = OwnedChild(
            Command::new(std::env::current_exe()?)
                .args(["--exact", "fault_worker", "--ignored", "--nocapture"])
                .env("HIVE_RECOVERY_FAULT", mode)
                .env("HIVE_RECOVERY_WITNESS", fixture.witness.to_string())
                .stdin(Stdio::from(OwnedFd::from(worker_end)))
                .stdout(Stdio::null())
                .spawn()?,
        );
        let mut byte = [0];
        control.read_exact(&mut byte)?;
        assert_eq!(&byte, b"R", "fault worker did not reach its checkpoint");
        Ok(Self { child, control })
    }

    fn kill(&mut self) -> Result<()> {
        assert!(
            self.child.0.try_wait()?.is_none(),
            "worker exited before SIGKILL"
        );
        self.child.0.kill()?;
        let status = until("SIGKILLed worker to exit", || {
            Ok(self.child.0.try_wait()?)
        })?;
        assert_eq!(status.signal(), Some(9));
        Ok(())
    }

    fn commit_and_exit(&mut self) -> Result<()> {
        self.control.write_all(b"C")?;
        let status = until("live worker to commit and exit", || {
            Ok(self.child.0.try_wait()?)
        })?;
        assert!(status.success(), "live fault worker failed ({status})");
        Ok(())
    }
}

fn until<T>(description: &str, mut attempt: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = attempt()? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {description}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "spawned only by the recovery tests"]
fn fault_worker() -> Result<()> {
    let mut control = UnixStream::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    control.set_read_timeout(Some(TIMEOUT))?;
    control.set_write_timeout(Some(TIMEOUT))?;
    let target = Target::from_fixture_env()?;
    let fixture = Fixture::attach(target, std::env::var("HIVE_RECOVERY_WITNESS")?.parse()?)?;
    let fault = match std::env::var("HIVE_RECOVERY_FAULT")?.as_str() {
        "before" => Fault::BeforeCommit,
        "after" => Fault::AfterCommit,
        "working" => Fault::WorkingSetOnly,
        "live" => Fault::LiveSessionAfterUnlock,
        _ => return Err("unknown fault mode".into()),
    };
    let outcome = fixture.submit_at(CLAIM, fault, || {
        control.write_all(b"R")?;
        let mut byte = [0];
        control.read_exact(&mut byte)?;
        assert_eq!(&byte, b"C");
        Ok(())
    })?;
    assert_eq!(
        fault,
        Fault::LiveSessionAfterUnlock,
        "unexpected worker return"
    );
    assert_eq!(outcome, Outcome::CommitUncertain);
    Ok(())
}

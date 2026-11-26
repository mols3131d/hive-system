//! A receipt protocol for the disposable, single-server recovery probe.

use hive_dolt_spike::{Target, begin, guard::FixtureWriter};
use mysql::prelude::Queryable;
use mysql::{Conn, Transaction};
use std::error::Error;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub const PENDING: &str = "recovery.pending";
const LOCK: &str = "recovery.lock";
const MAX_MARKER_BYTES: usize = 512;
pub const ISSUE: &str = "01900000-0000-7000-8000-000000000280";
pub const OTHER_ISSUE: &str = "01900000-0000-7000-8000-000000000281";
pub const BASE_REVISION: &str = "01900000-0000-7000-8000-000000000282";
pub const CLAIM_REVISION: &str = "01900000-0000-7000-8000-000000000283";
pub const EDIT_REVISION: &str = "01900000-0000-7000-8000-000000000284";
pub const CLAIM_ID: &str = "01900000-0000-7000-8000-000000000285";
pub const HOLDER: &str = "synthetic-holder";
const ACTOR: &str = "synthetic-actor";
const AREA: &str = "01900000-0000-7000-8000-000000000286";

#[derive(Clone, Copy)]
pub enum Change {
    Claim(&'static str),
    Rename(&'static str),
}

#[derive(Clone, Copy)]
pub struct Request {
    pub id: &'static str,
    pub issue: &'static str,
    pub expected_revision: &'static str,
    pub result_revision: &'static str,
    pub change: Change,
}

pub const CLAIM: Request = Request {
    id: "01900000-0000-7000-8000-000000000287",
    issue: ISSUE,
    expected_revision: BASE_REVISION,
    result_revision: CLAIM_REVISION,
    change: Change::Claim(HOLDER),
};

pub const ADVANCE: Request = Request {
    id: "01900000-0000-7000-8000-000000000288",
    issue: ISSUE,
    expected_revision: CLAIM_REVISION,
    result_revision: EDIT_REVISION,
    change: Change::Rename("Revised synthetic issue"),
};

pub const UNRELATED: Request = Request {
    id: "01900000-0000-7000-8000-000000000289",
    issue: OTHER_ISSUE,
    expected_revision: BASE_REVISION,
    ..ADVANCE
};

impl Request {
    fn payload(self) -> (&'static str, &'static str) {
        match self.change {
            Change::Claim(holder) => ("claim", holder),
            Change::Rename(title) => ("rename", title),
        }
    }

    fn result(self) -> String {
        match self.change {
            Change::Claim(_) => format!(
                r#"{{"issue_id":"{}","claim_id":"{CLAIM_ID}","revision":"{}"}}"#,
                self.issue, self.result_revision
            ),
            Change::Rename(_) => format!(
                r#"{{"issue_id":"{}","revision":"{}"}}"#,
                self.issue, self.result_revision
            ),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Receipt {
    pub kind: String,
    pub fingerprint: String,
    pub actor: String,
    pub base: String,
    pub result: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Confirmed {
    pub commit: String,
    pub receipt: Receipt,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Committed(Confirmed),
    Replayed(Confirmed),
    FingerprintConflict,
    StaleRevision,
    CommitUncertain,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Clear,
    RolledBack,
    Committed,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    BeforeCommit,
    AfterCommit,
    WorkingSetOnly,
    LiveSessionAfterUnlock,
}

pub struct Pending {
    pub session: u32,
    pub request: String,
    kind: String,
    fingerprint: String,
    pub base: String,
}

impl Pending {
    fn matches(&self, receipt: &Receipt) -> bool {
        receipt.fingerprint == self.fingerprint
            && receipt.kind == self.kind
            && receipt.base == self.base
            && receipt.actor == ACTOR
    }
}

pub struct Fixture {
    target: Target,
    nonce: String,
    pub witness: u32,
}

impl Fixture {
    pub fn start() -> Result<(Self, Conn)> {
        let target = Target::from_fixture_env()?;
        let mut connection = target.connect()?;
        let fixture = Self::attach(target, connection.connection_id())?;
        let acquired: Option<u8> =
            connection.exec_first("SELECT GET_LOCK(?, 0)", (&fixture.nonce,))?;
        assert_eq!(
            acquired,
            Some(1),
            "could not mark the fixture server instance"
        );
        Ok((fixture, connection))
    }

    pub fn attach(target: Target, witness: u32) -> Result<Self> {
        let directory = std::env::var("TMPDIR")?;
        if std::env::current_dir()? != Path::new(&directory).canonicalize()? {
            return Err("run the recovery test in its disposable fixture directory".into());
        }
        let nonce = std::env::var("HIVE_RECOVERY_NONCE")?;
        if !is_hex(&nonce, 32) {
            return Err("invalid fixture instance identifier".into());
        }
        Ok(Self {
            target,
            nonce,
            witness,
        })
    }

    fn verify_server(&self, connection: &mut Conn) -> Result<()> {
        let owner: Option<Option<u32>> =
            connection.exec_first("SELECT IS_USED_LOCK(?)", (&self.nonce,))?;
        if owner != Some(Some(self.witness)) {
            return Err("fixture server witness did not match".into());
        }
        Ok(())
    }

    pub fn reader(&self) -> Result<Conn> {
        let mut reader = self.target.connect()?;
        self.verify_server(&mut reader)?;
        Ok(reader)
    }

    fn writer(&self) -> Result<FixtureWriter> {
        let mut writer = FixtureWriter::acquire(
            &self.target,
            Path::new(LOCK),
            Duration::from_secs(2),
            |_| Ok(()),
        )?;
        self.verify_server(&mut writer.connection)?;
        Ok(writer)
    }

    pub fn seed(&self) -> Result<String> {
        let mut writer = self.writer()?;
        let mut seed = begin(&mut writer.connection, true)?;
        seed.exec_drop(
            "INSERT INTO areas (id, slug, name, revision) VALUES (?, 'recovery', 'Synthetic', ?)",
            (AREA, BASE_REVISION),
        )?;
        for (issue, number) in [(ISSUE, 1), (OTHER_ISSUE, 2)] {
            seed.exec_drop(
                "INSERT INTO issues (id, number, area_id, title, type, status, revision, created_at, updated_at)
                 VALUES (?, ?, ?, 'Synthetic', 'task', 'todo', ?, UTC_TIMESTAMP(6), UTC_TIMESTAMP(6))",
                (issue, number, AREA, BASE_REVISION),
            )?;
        }
        seed.commit()?;
        head(&mut self.reader()?)
    }

    fn binding(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.target.workspace_id,
            self.target.database,
            self.target.port,
            self.nonce,
            self.witness
        )
    }

    fn persist(&self, pending: &Pending) -> Result<()> {
        let temporary = "recovery.pending.tmp";
        let mut file = File::create(temporary)?;
        writeln!(
            file,
            "{}\n{} {} {} {} {}",
            self.binding(),
            pending.session,
            pending.request,
            pending.kind,
            pending.fingerprint,
            pending.base
        )?;
        file.sync_all()?;
        fs::rename(temporary, PENDING)?;
        File::open(".")?.sync_all()?;
        Ok(())
    }

    pub fn pending(&self) -> Result<Option<Pending>> {
        let file = match File::open(PENDING) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut text = String::new();
        file.take((MAX_MARKER_BYTES + 1) as u64)
            .read_to_string(&mut text)?;
        let (binding, fields) = text.split_once('\n').ok_or("invalid pending marker")?;
        if text.len() > MAX_MARKER_BYTES || binding != self.binding() {
            return Err("pending marker does not match the fixture".into());
        }
        let parts: Vec<_> = fields.split_ascii_whitespace().collect();
        let [session, request, kind, fingerprint, base] = parts.as_slice() else {
            return Err("invalid pending fields".into());
        };
        let session: u32 = session.parse()?;
        if session == 0
            || session == self.witness
            || request.len() != 36
            || !matches!(*kind, "claim" | "rename")
            || !is_hex(fingerprint, 64)
            || !is_hash(base)
        {
            return Err("invalid pending fields".into());
        }
        Ok(Some(Pending {
            session,
            request: (*request).to_owned(),
            kind: (*kind).to_owned(),
            fingerprint: (*fingerprint).to_owned(),
            base: (*base).to_owned(),
        }))
    }

    pub fn recover(&self) -> Result<Recovery> {
        let _writer = self.writer()?;
        self.reconcile()
    }

    fn reconcile(&self) -> Result<Recovery> {
        let Some(pending) = self.pending()? else {
            return Ok(Recovery::Clear);
        };
        let mut observer = self.reader()?;
        if session_present(&mut observer, pending.session)? {
            return Ok(Recovery::Uncertain);
        }

        // The independently witnessed SQL session has left this server's process list.
        // Read history on a fresh connection after that teardown observation.
        let mut reader = self.reader()?;
        let observed = head(&mut reader)?;
        if !clean(&mut reader)? {
            return Ok(Recovery::Uncertain);
        }
        let recovered = match confirmed_receipt(&mut reader, &observed, &pending.request)? {
            Some(confirmed) if pending.matches(&confirmed.receipt) => Recovery::Committed,
            None if observed == pending.base => Recovery::RolledBack,
            _ => return Ok(Recovery::Uncertain),
        };
        clear_pending()?;
        Ok(recovered)
    }

    pub fn submit(&self, request: Request) -> Result<Outcome> {
        self.submit_at(request, Fault::None, || Ok(()))
    }

    pub fn submit_at(
        &self,
        request: Request,
        fault: Fault,
        checkpoint: impl FnOnce() -> Result<()>,
    ) -> Result<Outcome> {
        let mut writer = self.writer()?;
        if self.reconcile()? == Recovery::Uncertain {
            return Ok(Outcome::CommitUncertain);
        }

        let mut reader = self.reader()?;
        let base = head(&mut reader)?;
        let (kind, value) = request.payload();
        // The fixture's normalized request fields are fixed strings without newlines.
        let normalized = format!(
            "{}\n{ACTOR}\n{kind}\n{}\n{}\n{value}",
            self.target.workspace_id, request.issue, request.expected_revision
        );
        let fingerprint: String = reader
            .exec_first("SELECT SHA2(?, 256)", (normalized,))?
            .ok_or("expected a request fingerprint")?;
        if let Some(confirmed) = confirmed_receipt(&mut reader, &base, request.id)? {
            return Ok(if confirmed.receipt.fingerprint == fingerprint {
                Outcome::Replayed(confirmed)
            } else {
                Outcome::FingerprintConflict
            });
        }
        if !clean(&mut reader)? {
            return Ok(Outcome::CommitUncertain);
        }
        let revision: Option<String> =
            reader.exec_first("SELECT revision FROM issues WHERE id = ?", (request.issue,))?;
        if revision.as_deref() != Some(request.expected_revision) {
            return Ok(Outcome::StaleRevision);
        }

        let session = writer.connection.connection_id();
        assert!(
            session_present(&mut reader, session)?,
            "writer must be visible to its observer"
        );
        let pending = Pending {
            session,
            request: request.id.to_owned(),
            kind: kind.to_owned(),
            fingerprint,
            base,
        };
        self.persist(&pending)?;

        if fault == Fault::LiveSessionAfterUnlock {
            // Keep the original SQL session alive while injecting loss of the OS guard.
            std::mem::swap(&mut writer.connection, &mut reader);
            let transaction = stage(&mut reader, request, &pending, true)?;
            drop(writer);
            checkpoint()?;
            transaction.commit()?;
            return Ok(Outcome::CommitUncertain);
        }

        let transaction = stage(
            &mut writer.connection,
            request,
            &pending,
            fault != Fault::WorkingSetOnly,
        )?;
        if fault == Fault::BeforeCommit {
            checkpoint()?;
            return Err("pre-COMMIT pause unexpectedly resumed".into());
        }
        transaction.commit()?;
        if fault != Fault::None {
            checkpoint()?;
            return Err("post-COMMIT pause unexpectedly resumed".into());
        }

        let mut reader = self.reader()?;
        let observed = head(&mut reader)?;
        let confirmed = confirmed_receipt(&mut reader, &observed, request.id)?
            .ok_or("SQL COMMIT did not publish a versioned receipt")?;
        if confirmed.commit != observed
            || !pending.matches(&confirmed.receipt)
            || confirmed.receipt.result != request.result()
            || !clean(&mut reader)?
        {
            return Ok(Outcome::CommitUncertain);
        }
        clear_pending()?;
        Ok(Outcome::Committed(confirmed))
    }
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_hash(hash: &str) -> bool {
    hash.len() == 32
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'v').contains(&b))
}

pub fn head(connection: &mut Conn) -> Result<String> {
    let hash: String = connection
        .query_first("SELECT DOLT_HASHOF('HEAD')")?
        .ok_or("expected Dolt HEAD")?;
    if !is_hash(&hash) {
        return Err("invalid Dolt commit hash".into());
    }
    Ok(hash)
}

pub fn clean(connection: &mut Conn) -> Result<bool> {
    Ok(connection.query_first("SELECT COUNT(*) FROM dolt_status")? == Some(0u64))
}

pub fn session_present(connection: &mut Conn, id: u32) -> Result<bool> {
    // In 2.7.6 the server tracks idle sessions here through SessionManager.RemoveConn.
    // Removal runs after the connection's command loop and session cleanup.
    let found: Option<u32> = connection.exec_first(
        "SELECT ID FROM information_schema.PROCESSLIST WHERE ID = ?",
        (id,),
    )?;
    Ok(found.is_some())
}

fn clear_pending() -> Result<()> {
    fs::remove_file(PENDING)?;
    File::open(".")?.sync_all()?;
    Ok(())
}

fn stage<'a>(
    connection: &'a mut Conn,
    request: Request,
    pending: &Pending,
    versioned: bool,
) -> Result<Transaction<'a>> {
    let mut transaction = begin(connection, versioned)?;
    match request.change {
        Change::Claim(holder) => {
            transaction.exec_drop(
                "UPDATE issues SET status = 'in-progress', revision = ?, updated_at = UTC_TIMESTAMP(6)
                 WHERE id = ? AND revision = ? AND status = 'todo'",
                (request.result_revision, request.issue, request.expected_revision),
            )?;
            assert_eq!(
                transaction.affected_rows(),
                1,
                "expected one claimable issue"
            );
            transaction.exec_drop(
                "INSERT INTO issue_claims (issue_id, claim_id, holder_ref, acquired_at)
                 VALUES (?, ?, ?, UTC_TIMESTAMP(6))",
                (request.issue, CLAIM_ID, holder),
            )?;
        }
        Change::Rename(title) => {
            transaction.exec_drop(
                "UPDATE issues SET title = ?, revision = ?, updated_at = UTC_TIMESTAMP(6)
                 WHERE id = ? AND revision = ?",
                (
                    title,
                    request.result_revision,
                    request.issue,
                    request.expected_revision,
                ),
            )?;
            assert_eq!(
                transaction.affected_rows(),
                1,
                "expected one editable issue"
            );
        }
    }
    transaction.exec_drop(
        "INSERT INTO operation_receipts
         (request_id, kind, fingerprint, actor_ref, authorization_ref, workflow_evidence_json,
          base_commit, result_json, created_at)
         VALUES (?, ?, ?, ?, 'synthetic-authorization', '{}', ?, ?, UTC_TIMESTAMP(6))",
        (
            request.id,
            &pending.kind,
            &pending.fingerprint,
            ACTOR,
            &pending.base,
            request.result(),
        ),
    )?;
    Ok(transaction)
}

fn receipt_at(connection: &mut Conn, commit: &str, request: &str) -> Result<Option<Receipt>> {
    // Dolt 2.7.6 requires a literal in AS OF.
    if !is_hash(commit) {
        return Err("invalid receipt snapshot hash".into());
    }
    let row: Option<(String, String, String, String, String)> = connection.exec_first(
        format!(
            "SELECT kind, fingerprint, actor_ref, base_commit, result_json
                 FROM operation_receipts AS OF '{commit}' WHERE request_id = ?"
        ),
        (request,),
    )?;
    Ok(row.map(|(kind, fingerprint, actor, base, result)| Receipt {
        kind,
        fingerprint,
        actor,
        base,
        result,
    }))
}

fn confirmed_receipt(
    connection: &mut Conn,
    observed: &str,
    request: &str,
) -> Result<Option<Confirmed>> {
    let Some(receipt) = receipt_at(connection, observed, request)? else {
        return Ok(None);
    };
    let mut commit = observed.to_owned();
    // The probe creates only a handful of linear commits and keeps receipts immutable.
    for _ in 0..8 {
        let parents: Vec<String> = connection.exec(
            "SELECT parent_hash FROM dolt_commit_ancestors WHERE commit_hash = ?",
            (&commit,),
        )?;
        let [parent] = parents.as_slice() else {
            return Err("receipt history is not linear".into());
        };
        match receipt_at(connection, parent, request)? {
            Some(previous) if previous == receipt => commit.clone_from(parent),
            None if parent == &receipt.base => return Ok(Some(Confirmed { commit, receipt })),
            _ => return Err("receipt history does not match its recorded base".into()),
        }
    }
    Err("receipt history exceeded the fixture bound".into())
}

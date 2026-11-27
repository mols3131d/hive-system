use crate::cli::is_slug;

use super::{Diagnostics, HiveError, Snapshot};

const TABLE: &str = "bots";

pub(super) fn validate(
    snapshot: &mut Snapshot,
    diagnostics: &mut Diagnostics,
) -> Result<(), HiveError> {
    let rows = snapshot.records::<(String, String)>(TABLE, "id, slug", "id")?;
    for (id, slug) in rows {
        validate_row(&id, &slug, diagnostics);
    }
    Ok(())
}

fn validate_row(id: &str, slug: &str, diagnostics: &mut Diagnostics) {
    diagnostics.check_id(TABLE, id, &[]);
    if !is_slug(slug) {
        diagnostics.add(
            "INVALID_BOT_SLUG",
            TABLE,
            &[id],
            "Use a lowercase kebab-case Bot slug.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_bot_ids_and_lowercase_kebab_case_slugs() {
        let valid_id = "01900000-0000-7000-8000-00000000000a";
        for slug in ["queen", "queen-1", "a-b-c-0", "01"] {
            let mut diagnostics = Diagnostics::default();
            validate_row(valid_id, slug, &mut diagnostics);
            assert!(diagnostics.items.is_empty(), "{slug:?}");
        }
    }

    #[test]
    fn rejects_malformed_ids_and_slugs_without_exposing_raw_values() {
        let valid_id = "01900000-0000-7000-8000-00000000000a";
        for invalid_slug in [
            "",
            "-queen",
            "queen-",
            "queen--alpha",
            "Queen-Alpha",
            "queen_alpha",
            "queen alpha",
            "../queen",
            "queen/soul",
            "café",
        ] {
            let mut diagnostics = Diagnostics::default();
            validate_row(valid_id, invalid_slug, &mut diagnostics);
            let codes: Vec<_> = diagnostics.items.iter().map(|item| item.code).collect();
            assert_eq!(codes, ["INVALID_BOT_SLUG"], "{invalid_slug:?}");
            assert_eq!(diagnostics.items[0].record_ids, [valid_id]);
            if !invalid_slug.is_empty() {
                assert!(!diagnostics.items[0].message.contains(invalid_slug));
            }
        }

        let uppercase_id = valid_id.to_ascii_uppercase();
        let uuidv4_id = valid_id.replace("-7000-", "-4000-");
        for bad_id in ["invalid-bot-id", uppercase_id.as_str(), uuidv4_id.as_str()] {
            let mut diagnostics = Diagnostics::default();
            validate_row(bad_id, "bad/slug", &mut diagnostics);
            let codes: Vec<_> = diagnostics.items.iter().map(|item| item.code).collect();
            assert_eq!(codes, ["INVALID_ID", "INVALID_BOT_SLUG"], "{bad_id:?}");
            for item in &diagnostics.items {
                assert!(item.record_ids.is_empty());
                assert!(!item.message.contains(bad_id));
                assert!(!item.message.contains("bad/slug"));
            }
        }
    }
}

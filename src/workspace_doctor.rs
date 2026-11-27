use std::path::Path;

use serde_json::Value;

use crate::cli::{CommonOptions, DoltTargetArgs, Format};
use crate::error::HiveError;

pub(crate) fn observe(
    options: &CommonOptions,
    target: Option<&Path>,
    endpoint: &DoltTargetArgs,
) -> (Format, Result<Value, HiveError>) {
    #[cfg(target_os = "linux")]
    {
        use serde_json::json;

        use crate::workspace_init::{inspect_empty_target, probe_versions};

        if let Some(target) = target {
            let result = (|| {
                let mut data = inspect_empty_target(target)?;
                data["initialized"] = json!(false);
                data["sources"] = json!({
                    "workspace": "cli",
                    "output_format": if options.format.is_some() { "cli" } else { "default" },
                });
                data["versions"] = probe_versions()?;
                data["deferred"] = json!({
                    "descriptor": "workspace_uninitialized",
                    "runtime": "workspace_uninitialized",
                    "committed_identity": "workspace_uninitialized"
                });
                Ok(data)
            })();
            return (options.format.unwrap_or_default(), result);
        }

        let (format, status) = crate::workspace_status::diagnose(options, endpoint);
        let result = status.and_then(|mut data| {
            data["initialized"] = json!(true);
            data["versions"] = probe_versions()?;
            if !endpoint.has_any() {
                data.as_object_mut().unwrap().remove("committed_identity");
                data["deferred"] =
                    json!({"committed_identity": "authenticated_endpoint_not_supplied"});
            }
            Ok(data)
        });
        (format, result)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (target, endpoint);
        (
            options.format.unwrap_or_default(),
            Err(HiveError::new(
                crate::error::ErrorCode::NotImplemented,
                "Workspace doctor is supported only on Linux.",
            )),
        )
    }
}

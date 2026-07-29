use std::collections::HashMap;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

const SERVICE: &str = "org.freedesktop.impl.portal.PermissionStore";
const OBJECT_PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
const INTERFACE: &str = "org.freedesktop.impl.portal.PermissionStore";
const LOCATION_TABLE: &str = "location";
const LOCATION_ID: &str = "location";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationPermissionRecord {
    pub app_id: String,
    pub accuracy: LocationAccuracy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationAccuracy {
    None,
    Country,
    City,
    Neighborhood,
    Street,
    Exact,
    Unknown(String),
}

impl LocationPermissionRecord {
    pub fn decision_label(&self) -> &'static str {
        match self.accuracy {
            LocationAccuracy::None => "Denied",
            LocationAccuracy::Unknown(_) => "Unknown",
            _ => "Allowed",
        }
    }

    pub fn accuracy_label(&self) -> String {
        match &self.accuracy {
            LocationAccuracy::None => "No location access".to_string(),
            LocationAccuracy::Country => "Country-level accuracy".to_string(),
            LocationAccuracy::City => "City-level accuracy".to_string(),
            LocationAccuracy::Neighborhood => "Neighborhood-level accuracy".to_string(),
            LocationAccuracy::Street => "Street-level accuracy".to_string(),
            LocationAccuracy::Exact => "Exact location".to_string(),
            LocationAccuracy::Unknown(value) => format!("Unknown accuracy ({value})"),
        }
    }
}

pub fn list_location_permission_records() -> Result<Vec<LocationPermissionRecord>, String> {
    let connection =
        Connection::session().map_err(|err| format!("connect to session bus: {err}"))?;
    let proxy = permission_store_proxy(&connection)?;
    let lookup: Result<(HashMap<String, Vec<String>>, OwnedValue), zbus::Error> =
        proxy.call("Lookup", &(LOCATION_TABLE, LOCATION_ID));

    match lookup {
        Ok((permissions, _data)) => Ok(parse_location_permissions(permissions)),
        Err(err) if is_not_found(&err) => Ok(Vec::new()),
        Err(err) => Err(format!("look up saved location permissions: {err}")),
    }
}

pub fn revoke_location_permission(record: &LocationPermissionRecord) -> Result<(), String> {
    let connection =
        Connection::session().map_err(|err| format!("connect to session bus: {err}"))?;
    let proxy = permission_store_proxy(&connection)?;
    proxy
        .call::<_, _, ()>(
            "DeletePermission",
            &(LOCATION_TABLE, LOCATION_ID, record.app_id.as_str()),
        )
        .map_err(|err| format!("revoke location permission for {}: {err}", record.app_id))
}

fn permission_store_proxy(connection: &Connection) -> Result<Proxy<'_>, String> {
    Proxy::new(connection, SERVICE, OBJECT_PATH, INTERFACE)
        .map_err(|err| format!("connect to XDG permission store: {err}"))
}

fn parse_location_permissions(
    permissions: HashMap<String, Vec<String>>,
) -> Vec<LocationPermissionRecord> {
    let mut records = permissions
        .into_iter()
        .map(|(app_id, values)| LocationPermissionRecord {
            app_id,
            accuracy: parse_accuracy(values.first().map(String::as_str)),
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| left.app_id.cmp(&right.app_id));
    records
}

fn parse_accuracy(value: Option<&str>) -> LocationAccuracy {
    match value {
        Some("NONE") => LocationAccuracy::None,
        Some("COUNTRY") => LocationAccuracy::Country,
        Some("CITY") => LocationAccuracy::City,
        Some("NEIGHBORHOOD") => LocationAccuracy::Neighborhood,
        Some("STREET") => LocationAccuracy::Street,
        Some("EXACT") => LocationAccuracy::Exact,
        Some(other) => LocationAccuracy::Unknown(other.to_string()),
        None => LocationAccuracy::Unknown("missing".to_string()),
    }
}

fn is_not_found(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.portal.Error.NotFound"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_sorts_location_permissions() {
        let permissions = HashMap::from([
            (
                "org.example.Weather".to_string(),
                vec!["CITY".to_string(), "123".to_string()],
            ),
            (
                "org.example.Maps".to_string(),
                vec!["EXACT".to_string(), "456".to_string()],
            ),
        ]);

        let records = parse_location_permissions(permissions);
        assert_eq!(records[0].app_id, "org.example.Maps");
        assert_eq!(records[0].accuracy, LocationAccuracy::Exact);
        assert_eq!(records[1].app_id, "org.example.Weather");
        assert_eq!(records[1].accuracy, LocationAccuracy::City);
    }

    #[test]
    fn preserves_saved_denials_and_malformed_entries_for_revocation() {
        let permissions = HashMap::from([
            ("org.example.Denied".to_string(), vec!["NONE".to_string()]),
            ("org.example.Broken".to_string(), Vec::new()),
        ]);

        let records = parse_location_permissions(permissions);
        assert_eq!(
            records[0].accuracy,
            LocationAccuracy::Unknown("missing".to_string())
        );
        assert_eq!(records[0].decision_label(), "Unknown");
        assert_eq!(records[1].accuracy, LocationAccuracy::None);
        assert_eq!(records[1].decision_label(), "Denied");
    }

    #[test]
    #[ignore = "requires an isolated session bus and xdg-permission-store"]
    fn permission_store_list_and_revoke_round_trip() {
        let connection = Connection::session().expect("connect to isolated session bus");
        let proxy = permission_store_proxy(&connection).expect("connect to permission store");
        let app_id = "org.focaldesk.LocationPermissionTest";
        proxy
            .call::<_, _, ()>(
                "SetPermission",
                &(
                    LOCATION_TABLE,
                    true,
                    LOCATION_ID,
                    app_id,
                    vec!["CITY", "123"],
                ),
            )
            .expect("seed location permission");

        let records = list_location_permission_records().expect("list location permissions");
        let record = records
            .iter()
            .find(|record| record.app_id == app_id)
            .expect("seeded record is visible");
        assert_eq!(record.accuracy, LocationAccuracy::City);

        revoke_location_permission(record).expect("revoke location permission");
        let records = list_location_permission_records().expect("list after revoke");
        assert!(records.iter().all(|record| record.app_id != app_id));
    }
}

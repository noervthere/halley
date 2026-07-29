use zbus::fdo;
use zbus::message::Header;
use zbus::names::{BusName, OwnedUniqueName, UniqueName, WellKnownName};

fn same_unique_name(sender: &UniqueName<'_>, owner: &UniqueName<'_>) -> bool {
    sender == owner
}

pub async fn require_name_owner(
    connection: &zbus::Connection,
    header: Header<'_>,
    authorized_name: &str,
    denial: &str,
) -> fdo::Result<OwnedUniqueName> {
    let sender = header
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("missing D-Bus sender".to_owned()))?;
    let proxy = fdo::DBusProxy::new(connection)
        .await
        .map_err(|err| fdo::Error::Failed(err.to_string()))?;
    let authorized_name = WellKnownName::try_from(authorized_name)
        .map_err(|err| fdo::Error::Failed(err.to_string()))?;
    let owner = proxy
        .get_name_owner(BusName::WellKnown(authorized_name))
        .await
        .map_err(|_| fdo::Error::AccessDenied(denial.to_owned()))?;
    if !same_unique_name(sender, &owner.as_ref()) {
        return Err(fdo::Error::AccessDenied(denial.to_owned()));
    }
    Ok(owner)
}

#[cfg(test)]
mod tests {
    use super::same_unique_name;
    use zbus::names::UniqueName;

    #[test]
    fn unique_name_comparison_rejects_a_different_bus_client() {
        let authorized = UniqueName::try_from(":1.42").unwrap();
        let other = UniqueName::try_from(":1.43").unwrap();
        assert!(same_unique_name(&authorized, &authorized));
        assert!(!same_unique_name(&other, &authorized));
    }
}

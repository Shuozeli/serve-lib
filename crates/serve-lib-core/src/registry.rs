use std::collections::BTreeMap;

use crate::{ListenerKey, MountId, NormalizedRoute, RouteMount, ServeError};

#[derive(Debug, Clone, Default)]
pub struct Registry {
    mounts_by_id: BTreeMap<MountId, RouteMount>,
    mounts_by_listener: BTreeMap<ListenerKey, BTreeMap<NormalizedRoute, MountId>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, mount: RouteMount) -> Result<(), ServeError> {
        if self.mounts_by_id.contains_key(&mount.id) {
            return Err(ServeError::RouteConflict(format!(
                "mount id already exists: {:?}",
                mount.id
            )));
        }

        let listener_routes = self
            .mounts_by_listener
            .entry(mount.listener.clone())
            .or_default();
        if listener_routes.contains_key(&mount.route) {
            return Err(ServeError::RouteConflict(format!(
                "route already exists on listener {}:{}: {}",
                mount.listener.bind_addr, mount.listener.port, mount.route
            )));
        }

        listener_routes.insert(mount.route.clone(), mount.id);
        self.mounts_by_id.insert(mount.id, mount);
        Ok(())
    }

    pub fn remove_by_id(&mut self, mount_id: MountId) -> Result<RouteMount, ServeError> {
        let mount = self
            .mounts_by_id
            .remove(&mount_id)
            .ok_or_else(|| ServeError::MountNotFound(format!("{mount_id:?}")))?;
        self.remove_route_index(&mount);
        Ok(mount)
    }

    pub fn remove_by_listener_route(
        &mut self,
        listener: &ListenerKey,
        route: &NormalizedRoute,
    ) -> Result<RouteMount, ServeError> {
        let mount_id = self
            .mounts_by_listener
            .get(listener)
            .and_then(|routes| routes.get(route))
            .copied()
            .ok_or_else(|| {
                ServeError::MountNotFound(format!(
                    "listener {}:{} route {}",
                    listener.bind_addr, listener.port, route
                ))
            })?;

        self.remove_by_id(mount_id)
    }

    pub fn get(&self, mount_id: MountId) -> Option<&RouteMount> {
        self.mounts_by_id.get(&mount_id)
    }

    pub fn mounts(&self) -> impl Iterator<Item = &RouteMount> {
        self.mounts_by_id.values()
    }

    pub fn listener_keys(&self) -> impl Iterator<Item = &ListenerKey> {
        self.mounts_by_listener.keys()
    }

    pub fn is_listener_empty(&self, listener: &ListenerKey) -> bool {
        self.mounts_by_listener
            .get(listener)
            .is_none_or(BTreeMap::is_empty)
    }

    pub fn match_request(
        &self,
        listener: &ListenerKey,
        request_path: &str,
    ) -> Option<RegistryRouteMatch<'_>> {
        let routes = self.mounts_by_listener.get(listener)?;

        routes.iter().rev().find_map(|(route, mount_id)| {
            route
                .matches_request_path(request_path)
                .and_then(|matched| {
                    self.mounts_by_id
                        .get(mount_id)
                        .map(|mount| RegistryRouteMatch {
                            mount,
                            relative_path: matched.relative_path,
                        })
                })
        })
    }

    fn remove_route_index(&mut self, mount: &RouteMount) {
        let should_remove_listener =
            if let Some(routes) = self.mounts_by_listener.get_mut(&mount.listener) {
                routes.remove(&mount.route);
                routes.is_empty()
            } else {
                false
            };

        if should_remove_listener {
            self.mounts_by_listener.remove(&mount.listener);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRouteMatch<'a> {
    pub mount: &'a RouteMount,
    pub relative_path: String,
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::path::PathBuf;

    use super::*;

    fn listener(port: u16) -> ListenerKey {
        ListenerKey {
            bind_addr: IpAddr::from([127, 0, 0, 1]),
            port,
        }
    }

    fn mount(listener: ListenerKey, route: &str, root: &str) -> RouteMount {
        RouteMount {
            id: MountId::new(),
            listener,
            route: route.parse().unwrap(),
            local_root: PathBuf::from(root),
            index_file: "index.html".to_string(),
            spa: false,
            render: Default::default(),
            readonly: true,
            expires_at: None,
            display_name: None,
        }
    }

    #[test]
    fn rejects_duplicate_route_on_same_listener() {
        let listener = listener(8088);
        let mut registry = Registry::new();
        registry
            .insert(mount(listener.clone(), "/app", "/srv/app-a"))
            .unwrap();

        let result = registry.insert(mount(listener, "/app/", "/srv/app-b"));
        assert!(matches!(result, Err(ServeError::RouteConflict(_))));
    }

    #[test]
    fn allows_same_route_on_different_listeners() {
        let mut registry = Registry::new();
        registry
            .insert(mount(listener(8088), "/app", "/srv/app-a"))
            .unwrap();
        registry
            .insert(mount(listener(8089), "/app", "/srv/app-b"))
            .unwrap();

        assert_eq!(registry.mounts().count(), 2);
    }

    #[test]
    fn longest_prefix_route_wins() {
        let listener = listener(8088);
        let mut registry = Registry::new();
        let app = mount(listener.clone(), "/app", "/srv/app");
        let api = mount(listener.clone(), "/app/api", "/srv/api");
        let api_id = api.id;
        registry.insert(app).unwrap();
        registry.insert(api).unwrap();

        let matched = registry
            .match_request(&listener, "/app/api/users.json")
            .unwrap();
        assert_eq!(matched.mount.id, api_id);
        assert_eq!(matched.relative_path, "users.json");
    }

    #[test]
    fn removing_last_mount_removes_listener_key() {
        let listener = listener(8088);
        let mut registry = Registry::new();
        let mount = mount(listener.clone(), "/app", "/srv/app");
        let mount_id = mount.id;
        registry.insert(mount).unwrap();

        registry.remove_by_id(mount_id).unwrap();

        assert!(registry.is_listener_empty(&listener));
        assert_eq!(registry.listener_keys().count(), 0);
    }
}

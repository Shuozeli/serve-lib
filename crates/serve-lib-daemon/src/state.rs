use std::collections::BTreeMap;

use serve_lib_core::{
    ListenerKey, MountId, Registry, RegistryRouteMatch, RouteMount, ServeError, TlsPolicy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateGeneration(u64);

impl StateGeneration {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Default)]
pub(crate) struct StateController {
    registry: Registry,
    listener_policies: BTreeMap<ListenerKey, TlsPolicy>,
    generation: u64,
}

impl StateController {
    pub fn generation(&self) -> StateGeneration {
        StateGeneration(self.generation)
    }

    pub fn mount_count(&self) -> usize {
        self.registry.mounts().count()
    }

    pub fn mounts(&self) -> impl Iterator<Item = &RouteMount> {
        self.registry.mounts()
    }

    pub fn listener_keys(&self) -> impl Iterator<Item = &ListenerKey> {
        self.registry.listener_keys()
    }

    pub fn is_listener_empty(&self, listener: &ListenerKey) -> bool {
        self.registry.is_listener_empty(listener)
    }

    pub fn validate_listener_policy(
        &self,
        listener: &ListenerKey,
        tls_policy: &TlsPolicy,
    ) -> Result<(), ServeError> {
        if let Some(existing) = self.listener_policies.get(listener) {
            if existing != tls_policy {
                return Err(ServeError::InvalidConfig(format!(
                    "listener {}:{} already has a different TLS policy",
                    listener.bind_addr, listener.port
                )));
            }
        }
        Ok(())
    }

    pub fn insert_mount(
        &mut self,
        mount: RouteMount,
        tls_policy: TlsPolicy,
    ) -> Result<StateGeneration, ServeError> {
        let listener = mount.listener.clone();
        self.registry.insert(mount)?;
        self.listener_policies.entry(listener).or_insert(tls_policy);
        Ok(self.bump_generation())
    }

    pub fn remove_by_listener_route(
        &mut self,
        listener: &ListenerKey,
        route: &serve_lib_core::NormalizedRoute,
    ) -> Result<(RouteMount, StateGeneration), ServeError> {
        let removed = self.registry.remove_by_listener_route(listener, route)?;
        let generation = self.bump_generation();
        Ok((removed, generation))
    }

    pub fn remove_by_id(
        &mut self,
        mount_id: MountId,
    ) -> Result<(RouteMount, StateGeneration), ServeError> {
        let removed = self.registry.remove_by_id(mount_id)?;
        let generation = self.bump_generation();
        Ok((removed, generation))
    }

    pub fn remove_listener_policy(&mut self, listener: &ListenerKey) {
        self.listener_policies.remove(listener);
    }

    pub fn match_request(
        &self,
        listener: &ListenerKey,
        request_path: &str,
    ) -> Option<RegistryRouteMatch<'_>> {
        self.registry.match_request(listener, request_path)
    }

    pub fn expired_mount_ids(&self, now: std::time::SystemTime) -> Vec<MountId> {
        self.registry
            .mounts()
            .filter(|mount| mount.expires_at.is_some_and(|expires_at| expires_at <= now))
            .map(|mount| mount.id)
            .collect()
    }

    fn bump_generation(&mut self) -> StateGeneration {
        self.generation = self.generation.saturating_add(1);
        self.generation()
    }
}

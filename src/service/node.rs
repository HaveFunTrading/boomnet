use crate::service::Handle;
use crate::service::endpoint::DisconnectReason;
use crate::service::select::SelectorToken;
use crate::service::time::TimeSource;
use std::time::Duration;

pub struct IONode<T, E> {
    pub target: T,
    pub endpoint: (Handle, E),
    pub ttl: Duration,
    pub disconnect_time_ns: u64,
    pub pending_disconnect: Option<DisconnectReason>,
}

/// Token-indexed storage for active I/O nodes.
pub struct IONodes<T, E> {
    slots: Vec<Option<IONode<T, E>>>,
}

const MIN_IO_NODE_SLOTS: usize = 4;

impl<T, E> Default for IONodes<T, E> {
    fn default() -> Self {
        Self {
            slots: Vec::with_capacity(MIN_IO_NODE_SLOTS),
        }
    }
}

impl<T, E> IONodes<T, E> {
    pub fn insert(&mut self, token: SelectorToken, node: IONode<T, E>) -> Result<(), IONode<T, E>> {
        let index = token as usize;
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || None);
        }
        let slot = &mut self.slots[index];
        if slot.is_some() {
            return Err(node);
        }
        *slot = Some(node);
        Ok(())
    }

    #[inline]
    pub fn get(&self, token: SelectorToken) -> Option<&IONode<T, E>> {
        self.slots.get(token as usize)?.as_ref()
    }

    #[inline]
    pub fn get_mut(&mut self, token: SelectorToken) -> Option<&mut IONode<T, E>> {
        self.slots.get_mut(token as usize)?.as_mut()
    }

    pub fn remove(&mut self, token: SelectorToken) -> Option<IONode<T, E>> {
        let node = self.slots.get_mut(token as usize)?.take()?;
        while self.slots.last().is_some_and(Option::is_none) {
            self.slots.pop();
        }
        Some(node)
    }

    #[inline]
    pub fn values(&self) -> impl Iterator<Item = &IONode<T, E>> {
        self.slots.iter().flatten()
    }

    #[inline]
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut IONode<T, E>> {
        self.slots.iter_mut().flatten()
    }

    #[inline]
    pub fn slots_mut(&mut self) -> std::slice::IterMut<'_, Option<IONode<T, E>>> {
        self.slots.iter_mut()
    }
}

impl<T, E> IONode<T, E> {
    pub fn new<TS>(target: T, handle: Handle, endpoint: E, ttl: Option<Duration>, ts: &TS) -> IONode<T, E>
    where
        TS: TimeSource,
    {
        let ttl = ttl.map_or(u64::MAX, |ttl| ttl.as_nanos() as u64);
        Self {
            target,
            endpoint: (handle, endpoint),
            ttl: Duration::from_nanos(ttl),
            disconnect_time_ns: ts.current_time_nanos().saturating_add(ttl),
            pending_disconnect: None,
        }
    }

    #[inline]
    pub const fn as_parts(&self) -> (&T, &(Handle, E)) {
        (&self.target, &self.endpoint)
    }

    #[inline]
    pub const fn as_parts_mut(&mut self) -> (&mut T, &mut (Handle, E)) {
        (&mut self.target, &mut self.endpoint)
    }

    #[inline]
    pub const fn as_target(&self) -> &T {
        &self.target
    }

    #[inline]
    pub const fn as_target_mut(&mut self) -> &mut T {
        &mut self.target
    }

    #[inline]
    pub const fn as_endpoint(&self) -> &(Handle, E) {
        &self.endpoint
    }

    #[inline]
    pub const fn as_endpoint_mut(&mut self) -> &mut (Handle, E) {
        &mut self.endpoint
    }

    #[inline]
    pub fn into_endpoint(self) -> (Handle, E) {
        self.endpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedTime;

    impl TimeSource for FixedTime {
        fn current_time_nanos(&self) -> u64 {
            0
        }
    }

    #[test]
    fn storage_is_token_indexed_and_compacts_trailing_holes() {
        let mut nodes = IONodes::<(), ()>::default();
        assert_eq!(nodes.slots.len(), 0);
        assert_eq!(nodes.slots.capacity(), MIN_IO_NODE_SLOTS);

        let node = IONode::new((), Handle(4), (), None, &FixedTime);
        assert!(nodes.insert(4, node).is_ok());
        assert_eq!(nodes.slots.len(), 5);
        assert!(nodes.get(4).is_some());

        assert!(nodes.remove(4).is_some());
        assert_eq!(nodes.slots.len(), 0);
    }
}

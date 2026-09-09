use std::io;
use std::time::Duration;

use crate::service::Handle;
use crate::service::endpoint::DisconnectReason;
use crate::service::select::{ActiveEndpointLookup, SelectorToken};

// Split the platform's token width between the registration index and connection generation.
// This keeps generated tokens representable by Mio's usize token on both 32- and 64-bit hosts.
const HANDLE_BITS: u32 = usize::BITS / 2;
const MAX_IDENTIFIER: u32 = u32::MAX >> (32 - HANDLE_BITS);

pub struct Registration<E, F, Q> {
    pub handle: Handle,
    pub factory: F,
    pub state: EndpointState<E, Q>,
    next_generation: u32,
}

pub enum EndpointState<E, Q> {
    Pending { query: Q, query_started_ns: u64 },
    Active(ActiveState<E>),
}

pub struct ActiveState<E> {
    pub token: SelectorToken,
    pub endpoint: E,
    pub ttl: Duration,
    pub disconnect_time_ns: u64,
    pub pending_disconnect: Option<DisconnectReason>,
}

impl<E, F, Q> Registration<E, F, Q> {
    pub fn active(&self) -> Option<&ActiveState<E>> {
        match &self.state {
            EndpointState::Active(active) => Some(active),
            EndpointState::Pending { .. } => None,
        }
    }

    pub fn active_mut(&mut self) -> Option<&mut ActiveState<E>> {
        match &mut self.state {
            EndpointState::Active(active) => Some(active),
            EndpointState::Pending { .. } => None,
        }
    }

    pub fn next_token(&mut self) -> io::Result<SelectorToken> {
        let generation = self.next_generation;
        self.next_generation = generation
            .checked_add(1)
            .filter(|next| u64::from(*next) < (1u64 << HANDLE_BITS))
            .ok_or_else(|| io::Error::other("endpoint connection generation exhausted"))?;
        Ok((u64::from(generation) << HANDLE_BITS) | u64::from(self.handle.0))
    }
}

/// Registrations indexed by handle. Empty slots preserve indices across removals.
/// A registration keeps its slot and factory across reconnects; only its state changes.
pub struct Registrations<E, F, Q> {
    slots: Vec<Option<Registration<E, F, Q>>>,
    next_handle: u32,
}

impl<E, F, Q> Default for Registrations<E, F, Q> {
    fn default() -> Self {
        Self {
            slots: Vec::with_capacity(4),
            next_handle: 0,
        }
    }
}

impl<E, F, Q> Registrations<E, F, Q> {
    pub fn allocate_handle(&mut self) -> io::Result<Handle> {
        let handle = Handle(self.next_handle);
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .filter(|next| u64::from(*next) < (1u64 << HANDLE_BITS))
            .ok_or_else(|| io::Error::other("registration handles exhausted"))?;
        Ok(handle)
    }

    pub fn insert(&mut self, handle: Handle, factory: F, query: Q, query_started_ns: u64) {
        let index = handle.0 as usize;
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || None);
        }
        assert!(self.slots[index].is_none(), "registration handle is already occupied");
        self.slots[index] = Some(Registration {
            handle,
            factory,
            state: EndpointState::Pending {
                query,
                query_started_ns,
            },
            next_generation: 0,
        });
    }

    #[inline]
    pub fn get(&self, handle: Handle) -> Option<&Registration<E, F, Q>> {
        self.slots.get(handle.0 as usize)?.as_ref()
    }

    #[inline]
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut Registration<E, F, Q>> {
        self.slots.get_mut(handle.0 as usize)?.as_mut()
    }

    pub fn remove(&mut self, handle: Handle) -> Option<Registration<E, F, Q>> {
        let registration = self.slots.get_mut(handle.0 as usize)?.take()?;
        while self.slots.last().is_some_and(Option::is_none) {
            self.slots.pop();
        }
        Some(registration)
    }

    #[inline]
    pub fn values(&self) -> impl Iterator<Item = &Registration<E, F, Q>> {
        self.slots.iter().flatten()
    }

    #[inline]
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Registration<E, F, Q>> {
        self.slots.iter_mut().flatten()
    }

    #[inline]
    pub fn slots_mut(&mut self) -> std::slice::IterMut<'_, Option<Registration<E, F, Q>>> {
        self.slots.iter_mut()
    }

    // Changing resolver types requires rebuilding pending queries; callers currently reset all
    // registrations. Preserve the handle counter so old selector tokens can never alias new ones.
    pub fn reset_with_query<R>(&self) -> Registrations<E, F, R> {
        Registrations {
            slots: Vec::with_capacity(4),
            next_handle: self.next_handle,
        }
    }
}

impl<E, F, Q> ActiveEndpointLookup<E> for Registrations<E, F, Q> {
    #[inline]
    fn get_active_mut(&mut self, token: SelectorToken) -> Option<&mut E> {
        let active = self
            .get_mut(Handle((token & u64::from(MAX_IDENTIFIER)) as u32))?
            .active_mut()?;
        (active.token == token).then_some(&mut active.endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activate(registration: &mut Registration<u32, &'static str, ()>, endpoint: u32) -> SelectorToken {
        let token = registration.next_token().unwrap();
        registration.state = EndpointState::Active(ActiveState {
            token,
            endpoint,
            ttl: Duration::from_secs(1),
            disconnect_time_ns: 1_000_000_000,
            pending_disconnect: None,
        });
        token
    }

    #[test]
    fn stale_tokens_cannot_access_pending_reconnected_or_removed_endpoints() {
        let mut registrations = Registrations::default();
        let first = registrations.allocate_handle().unwrap();
        let second = registrations.allocate_handle().unwrap();
        registrations.insert(first, "first factory", (), 0);
        registrations.insert(second, "second factory", (), 0);
        let old_token = activate(registrations.get_mut(first).unwrap(), 1);
        let other_token = activate(registrations.get_mut(second).unwrap(), 2);
        assert_eq!(registrations.get_active_mut(old_token), Some(&mut 1));

        registrations.get_mut(first).unwrap().state = EndpointState::Pending {
            query: (),
            query_started_ns: 1,
        };
        assert!(registrations.get_active_mut(old_token).is_none());
        let new_token = activate(registrations.get_mut(first).unwrap(), 3);
        assert_ne!(old_token, new_token);
        assert_eq!(registrations.get(first).unwrap().factory, "first factory");
        assert!(registrations.get_active_mut(old_token).is_none());
        assert_eq!(registrations.get_active_mut(new_token), Some(&mut 3));

        registrations.remove(first).unwrap();
        assert!(registrations.slots[first.0 as usize].is_none());
        assert!(registrations.get_active_mut(new_token).is_none());
        assert_eq!(registrations.get_active_mut(other_token), Some(&mut 2));
        registrations.remove(second).unwrap();
        assert!(registrations.slots.is_empty());

        let third = registrations.allocate_handle().unwrap();
        assert_ne!(third, first);
        assert_ne!(third, second);
        registrations.insert(third, "third factory", (), 0);
        activate(registrations.get_mut(third).unwrap(), 4);
        assert!(registrations.get_active_mut(old_token).is_none());
        assert!(registrations.get_active_mut(new_token).is_none());
        assert!(registrations.get_active_mut(other_token).is_none());
    }

    #[test]
    fn exhausted_identifiers_never_wrap_to_live_identifiers() {
        let mut registrations: Registrations<u32, &'static str, ()> = Registrations::default();
        let handle = registrations.allocate_handle().unwrap();
        registrations.insert(handle, "factory", (), 0);
        let registration = registrations.get_mut(handle).unwrap();
        registration.next_generation = u32::MAX;
        assert!(registration.next_token().is_err());
        assert!(registration.next_token().is_err());
        registrations.next_handle = u32::MAX;
        assert!(registrations.allocate_handle().is_err());
        assert!(registrations.allocate_handle().is_err());
        assert_eq!(registrations.get(handle).unwrap().factory, "factory");
    }
}

/// Per-slot state for a capture ring, dropped whenever the ring is reallocated.
pub struct SlotCache<T> {
    generation: u64,
    slots: Vec<Option<T>>,
}

impl<T> SlotCache<T> {
    pub const fn new() -> Self {
        Self {
            generation: 0,
            slots: Vec::new(),
        }
    }

    /// The state of `slot` in ring `generation`, created by `create` when missing or stale.
    pub fn get_or_try_insert<E>(
        &mut self,
        slot: usize,
        generation: u64,
        create: impl FnOnce() -> Result<T, E>,
    ) -> Result<&mut T, E> {
        if self.generation != generation {
            self.slots.clear();
            self.generation = generation;
        }
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, || None);
        }
        let entry = &mut self.slots[slot];
        let value = match entry.take() {
            Some(value) => value,
            None => create()?,
        };
        Ok(entry.insert(value))
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;

    fn lookup(cache: &mut SlotCache<u32>, slot: usize, generation: u64, created: &mut u32) -> u32 {
        let Ok(value) = cache.get_or_try_insert(slot, generation, || {
            *created += 1;
            Ok::<_, Infallible>(*created)
        });
        *value
    }

    #[test]
    fn creates_each_slot_once_per_generation() {
        let mut cache = SlotCache::new();
        let mut created = 0;
        assert_eq!(lookup(&mut cache, 2, 1, &mut created), 1);
        assert_eq!(lookup(&mut cache, 0, 1, &mut created), 2);
        assert_eq!(lookup(&mut cache, 2, 1, &mut created), 1);
        assert_eq!(lookup(&mut cache, 0, 1, &mut created), 2);
        assert_eq!(created, 2);
    }

    #[test]
    fn recreates_every_slot_after_reallocation() {
        let mut cache = SlotCache::new();
        let mut created = 0;
        lookup(&mut cache, 0, 1, &mut created);
        lookup(&mut cache, 1, 1, &mut created);
        assert_eq!(lookup(&mut cache, 1, 2, &mut created), 3);
        assert_eq!(lookup(&mut cache, 0, 2, &mut created), 4);
        assert_eq!(cache.slots.len(), 2);
    }

    #[test]
    fn retries_after_failed_creation() {
        let mut cache = SlotCache::<u32>::new();
        assert!(cache.get_or_try_insert(0, 0, || Err(())).is_err());
        assert_eq!(cache.get_or_try_insert(0, 0, || Ok::<_, ()>(7)), Ok(&mut 7));
    }
}

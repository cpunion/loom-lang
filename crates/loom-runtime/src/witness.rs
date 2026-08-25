//! Compact immutable conformance proofs shared by generated code and runtime.
//!
//! Compiler globals and synchronous stack applications use the same two-word
//! [`LoomWitnessInstance`] ABI as runtime-owned proofs. Runtime arenas keep the
//! instance address stable; only the GC arena decides reachability and sweeps
//! allocations. Cloning is transactional and uses a per-operation source map,
//! so a later stack allocation reusing an address can never alias an older
//! capture.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::ptr;

use loom_runtime_abi::{LoomWitnessDescriptor, LoomWitnessInstance};

const MAX_WITNESS_INSTANCES: usize = 1 << 20;
const MAX_WITNESS_FIELDS: usize = 1 << 20;

struct OwnedWitnessInstance {
    instance: LoomWitnessInstance,
    prerequisites: Box<[*const LoomWitnessInstance]>,
}

impl OwnedWitnessInstance {
    fn new(descriptor: *const LoomWitnessDescriptor, prerequisite_count: usize) -> Self {
        let prerequisites =
            vec![ptr::null::<LoomWitnessInstance>(); prerequisite_count].into_boxed_slice();
        let prerequisite_pointer = if prerequisites.is_empty() {
            ptr::null()
        } else {
            prerequisites.as_ptr()
        };
        Self {
            instance: LoomWitnessInstance {
                descriptor,
                prerequisites: prerequisite_pointer,
            },
            prerequisites,
        }
    }

    fn pointer(&self) -> *const LoomWitnessInstance {
        &raw const self.instance
    }

    fn allocation_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(
            self.prerequisites
                .len()
                .saturating_mul(size_of::<*const LoomWitnessInstance>()),
        )
    }
}

/// A fully validated clone which has not yet been published into an owner.
pub(crate) struct StagedWitnesses {
    roots: Box<[*const LoomWitnessInstance]>,
    allocations: Vec<Box<OwnedWitnessInstance>>,
}

impl StagedWitnesses {
    #[cfg(test)]
    pub(crate) fn roots(&self) -> &[*const LoomWitnessInstance] {
        &self.roots
    }

    pub(crate) fn allocation_bytes(&self) -> usize {
        self.allocations
            .iter()
            .map(|allocation| allocation.allocation_bytes())
            .fold(0_usize, usize::saturating_add)
    }

    fn into_parts(
        self,
    ) -> (
        Box<[*const LoomWitnessInstance]>,
        Vec<Box<OwnedWitnessInstance>>,
    ) {
        (self.roots, self.allocations)
    }
}

/// Stable non-moving storage for compact witness instances.
#[derive(Default)]
pub(crate) struct WitnessArena {
    allocations: Vec<Box<OwnedWitnessInstance>>,
}

impl WitnessArena {
    pub(crate) fn len(&self) -> usize {
        self.allocations.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.allocations.is_empty()
    }

    pub(crate) fn allocation_bytes(&self) -> usize {
        self.allocations
            .iter()
            .map(|allocation| allocation.allocation_bytes())
            .fold(0_usize, usize::saturating_add)
    }

    pub(crate) fn addresses(&self) -> impl Iterator<Item = usize> + '_ {
        self.allocations
            .iter()
            .map(|allocation| allocation.pointer() as usize)
    }

    pub(crate) fn retain_marked(&mut self, marked: &HashSet<usize>) -> usize {
        let before = self.allocations.len();
        self.allocations
            .retain(|allocation| marked.contains(&(allocation.pointer() as usize)));
        before.saturating_sub(self.allocations.len())
    }

    pub(crate) fn adopt(&mut self, staged: StagedWitnesses) -> Box<[*const LoomWitnessInstance]> {
        let (roots, allocations) = staged.into_parts();
        self.allocations.extend(allocations);
        roots
    }
}

unsafe fn descriptor_shape(descriptor: *const LoomWitnessDescriptor) -> Option<(usize, usize)> {
    let descriptor = unsafe { descriptor.as_ref() }?;
    let prerequisite_count = usize::try_from(descriptor.prerequisite_count).ok()?;
    let method_count = usize::try_from(descriptor.method_count).ok()?;
    if prerequisite_count > MAX_WITNESS_FIELDS
        || method_count > MAX_WITNESS_FIELDS
        || (method_count != 0 && descriptor.methods.is_null())
    {
        return None;
    }
    Some((prerequisite_count, method_count))
}

unsafe fn allocate_clone(
    source: *const LoomWitnessInstance,
    clones: &mut HashMap<usize, *const LoomWitnessInstance>,
    allocations: &mut Vec<Box<OwnedWitnessInstance>>,
    pending: &mut Vec<(*const LoomWitnessInstance, *const LoomWitnessInstance)>,
) -> Option<*const LoomWitnessInstance> {
    if source.is_null() {
        return None;
    }
    if let Some(clone) = clones.get(&(source as usize)).copied() {
        return Some(clone);
    }
    if allocations.len() == MAX_WITNESS_INSTANCES {
        return None;
    }
    let source_ref = unsafe { &*source };
    let (prerequisite_count, _) = unsafe { descriptor_shape(source_ref.descriptor) }?;
    if prerequisite_count != 0 && source_ref.prerequisites.is_null() {
        return None;
    }
    let allocation = Box::new(OwnedWitnessInstance::new(
        source_ref.descriptor,
        prerequisite_count,
    ));
    let clone = allocation.pointer();
    allocations.push(allocation);
    clones.insert(source as usize, clone);
    pending.push((source, clone));
    Some(clone)
}

/// Deep-clones one or more proof roots without publishing partial state.
///
/// The worklist preserves prerequisite order and sharing within this capture.
/// It is deliberately iterative so a compiler defect cannot overflow the
/// runtime stack. The caller must guarantee that every source pointer and its
/// descriptor remain live for this call.
pub(crate) unsafe fn clone_witnesses(
    sources: &[*const LoomWitnessInstance],
) -> Option<StagedWitnesses> {
    if sources.len() > MAX_WITNESS_FIELDS {
        return None;
    }
    let mut clones = HashMap::new();
    let mut allocations = Vec::new();
    let mut pending = Vec::new();
    let mut roots = Vec::with_capacity(sources.len());
    for source in sources {
        roots
            .push(unsafe { allocate_clone(*source, &mut clones, &mut allocations, &mut pending)? });
    }

    while let Some((source, clone)) = pending.pop() {
        let source_ref = unsafe { &*source };
        let (prerequisite_count, _) = unsafe { descriptor_shape(source_ref.descriptor) }?;
        let source_prerequisites = if prerequisite_count == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(source_ref.prerequisites, prerequisite_count) }
        };
        let clone_prerequisites = unsafe { (*clone).prerequisites.cast_mut() };
        for (index, source_prerequisite) in source_prerequisites.iter().copied().enumerate() {
            let prerequisite = unsafe {
                allocate_clone(
                    source_prerequisite,
                    &mut clones,
                    &mut allocations,
                    &mut pending,
                )?
            };
            unsafe { clone_prerequisites.add(index).write(prerequisite) };
        }
    }

    Some(StagedWitnesses {
        roots: roots.into_boxed_slice(),
        allocations,
    })
}

/// Iterates one immutable proof graph exactly once per instance address.
///
/// External instances (compiler globals, stack applications, and Task arena
/// captures) are visited as well as GC-owned nodes. This lets a root outside
/// the heap retain any GC-owned descendants. `false` reports an invalid ABI
/// shape; generated checked code must never produce it.
pub(crate) unsafe fn walk_witnesses(
    root: *const LoomWitnessInstance,
    mut visit: impl FnMut(*const LoomWitnessInstance),
) -> bool {
    if root.is_null() {
        return false;
    }
    let mut seen = HashSet::new();
    let mut pending = vec![root];
    while let Some(instance) = pending.pop() {
        if instance.is_null() || !seen.insert(instance as usize) {
            continue;
        }
        if seen.len() > MAX_WITNESS_INSTANCES {
            return false;
        }
        let instance_ref = unsafe { &*instance };
        let Some((prerequisite_count, _)) = (unsafe { descriptor_shape(instance_ref.descriptor) })
        else {
            return false;
        };
        if prerequisite_count != 0 && instance_ref.prerequisites.is_null() {
            return false;
        }
        visit(instance);
        if prerequisite_count != 0 {
            let prerequisites = unsafe {
                std::slice::from_raw_parts(instance_ref.prerequisites, prerequisite_count)
            };
            if prerequisites
                .iter()
                .any(|prerequisite| prerequisite.is_null())
            {
                return false;
            }
            pending.extend(prerequisites.iter().rev().copied());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;

    fn descriptor(prerequisite_count: u64) -> LoomWitnessDescriptor {
        LoomWitnessDescriptor {
            prerequisite_count,
            method_count: 0,
            methods: ptr::null::<*const c_void>(),
        }
    }

    #[test]
    fn clone_is_compact_ordered_and_preserves_shared_subproofs() {
        let leaf_descriptor = descriptor(0);
        let pair_descriptor = descriptor(2);
        let leaf = LoomWitnessInstance {
            descriptor: &raw const leaf_descriptor,
            prerequisites: ptr::null(),
        };
        let prerequisites = [&raw const leaf, &raw const leaf];
        let pair = LoomWitnessInstance {
            descriptor: &raw const pair_descriptor,
            prerequisites: prerequisites.as_ptr(),
        };

        let staged = unsafe { clone_witnesses(&[&raw const pair]) }.expect("clone proof DAG");
        assert_eq!(staged.allocations.len(), 2);
        let root = staged.roots()[0];
        assert_ne!(root, &raw const pair);
        let cloned_prerequisites = unsafe { std::slice::from_raw_parts((*root).prerequisites, 2) };
        assert_eq!(cloned_prerequisites[0], cloned_prerequisites[1]);
        assert_ne!(cloned_prerequisites[0], &raw const leaf);
    }

    #[test]
    fn invalid_clone_does_not_publish_partial_arena_state() {
        let invalid_descriptor = descriptor(1);
        let invalid = LoomWitnessInstance {
            descriptor: &raw const invalid_descriptor,
            prerequisites: ptr::null(),
        };
        let mut arena = WitnessArena::default();
        let staged = unsafe { clone_witnesses(&[&raw const invalid]) };
        assert!(staged.is_none());
        assert!(arena.is_empty());

        let leaf_descriptor = descriptor(0);
        let leaf = LoomWitnessInstance {
            descriptor: &raw const leaf_descriptor,
            prerequisites: ptr::null(),
        };
        let staged = unsafe { clone_witnesses(&[&raw const leaf]) }.expect("clone valid leaf");
        let roots = arena.adopt(staged);
        assert_eq!(roots.len(), 1);
        assert_eq!(arena.len(), 1);
    }
}

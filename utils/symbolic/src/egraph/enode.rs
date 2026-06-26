use std::fmt::Debug;
use std::hash::Hash;

pub trait ENode: Debug + Clone + Eq + Ord + Hash {
    fn matches(&self, other: &Self) -> bool;
    fn hash_cons(&self) -> u64;
}

#[derive(Debug, Eq)]
pub(crate) struct HashConsed<N: ENode>(pub N);

impl<N: ENode> Hash for HashConsed<N> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.hash_cons());
    }
}

impl<N: ENode> From<N> for HashConsed<N> {
    fn from(value: N) -> Self {
        Self(value)
    }
}

impl<N: ENode> PartialEq for HashConsed<N> {
    fn eq(&self, other: &Self) -> bool {
        self.0.hash_cons() == other.0.hash_cons()
    }
}

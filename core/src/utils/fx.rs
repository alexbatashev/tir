//! Hash containers for keys that are already well-mixed ids: the default
//! hasher's resistance to chosen keys costs more than a compiler's own
//! tables gain from it.

pub type HashMap<K, V> = std::collections::HashMap<K, V, tir_adt::FxBuildHasher>;
pub type HashSet<K> = std::collections::HashSet<K, tir_adt::FxBuildHasher>;

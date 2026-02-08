pub trait Commutative {
    fn is_commutative(&self) -> bool {
        true
    }
}

pub trait Terminator {
    fn is_terminator(&self) -> bool {
        true
    }
}

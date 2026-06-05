pub trait HasId {
    fn id(&self) -> &str;
}

pub fn find_backend<'a, T: HasId>(backends: &'a [T], id: &str) -> Option<&'a T> {
    backends.iter().find(|b| b.id() == id)
}

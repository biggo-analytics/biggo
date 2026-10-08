use std::collections::HashMap;
use std::sync::Arc;

/// An interned identifier. Two symbols from the same `Interner` are equal exactly when their
/// names are equal, so names compare as integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Symbol(u32);

impl Symbol {
    /// A dense index, usable as a key into a table of per-name data.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Default)]
pub struct Interner {
    symbols: HashMap<Arc<str>, Symbol>,
    names: Vec<Arc<str>>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, name: &str) -> Symbol {
        if let Some(&symbol) = self.symbols.get(name) {
            return symbol;
        }
        let symbol = Symbol(self.names.len() as u32);
        let name: Arc<str> = name.into();
        self.names.push(name.clone());
        self.symbols.insert(name, symbol);
        symbol
    }

    pub fn resolve(&self, symbol: Symbol) -> &str {
        &self.names[symbol.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_gives_same_symbol() {
        let mut interner = Interner::new();
        let qty = interner.intern("qty");
        let price = interner.intern("price");
        assert_eq!(qty, interner.intern("qty"));
        assert_ne!(qty, price);
        assert_eq!(interner.resolve(price), "price");
    }
}

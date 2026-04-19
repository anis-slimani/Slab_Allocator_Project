# Allocateur Slab en Rust

Un allocateur slab inspiré de l'allocateur SLUB du kernel Linux, écrit en Rust avec support `no_std`.

**Auteurs :** Angelov Onur, Slimani Anis — 4SIJ2

---

## Description

Implémente un allocateur slab utilisable comme `#[global_allocator]` dans un kernel Rust bare-metal. Au lieu d'appeler un allocateur générique pour chaque petit objet, il prédécoupe des pages physiques en slots de taille fixe et maintient une free-list par classe de taille.

9 classes de taille : **8, 16, 32, 64, 128, 256, 512, 1024, 2048** octets.

---

## Structure du projet

```
src/
├── lib.rs            — API publique, classes de taille, align_up
├── page_provider.rs  — trait PageProvider, StaticPageProvider, TestPageProvider
├── freelist.rs       — free-list LIFO intrusive
├── slab.rs           — page découpée en objets de taille fixe
├── cache.rs          — liste de slabs par classe + slab coloring
├── allocator.rs      — router un Layout vers le bon cache, statistiques
├── spinlock.rs       — Mutex spinlock (sans dépendance externe)
└── global.rs         — LockedAllocator implémentant GlobalAlloc
tests/
└── integration.rs    — tests de bout en bout
```

---

## Utilisation

### Comme allocateur global dans un kernel

```rust
use slab_allocator::LockedAllocator;

#[global_allocator]
static ALLOCATOR: LockedAllocator<256> = LockedAllocator::new();

fn kernel_main() {
    ALLOCATOR.init(); // à appeler avant tout usage du tas
    // Box, Vec, String fonctionnent désormais
}
```

Le paramètre const `256` correspond au nombre de pages de 4 KiB disponibles.

### En standalone (avec std, pour les tests)

```rust
use slab_allocator::{SlabAllocator, page_provider::TestPageProvider};

let provider = TestPageProvider::new();
let mut allocator = SlabAllocator::new(provider);
let ptr = allocator.alloc(Layout::from_size_align(32, 8).unwrap());
```

---

## Compilation & tests

```bash
# lancer tous les tests (unitaires + intégration + doc-tests)
cargo test

# générer la documentation
cargo doc --open

# compiler sans std (bare-metal)
cargo build --no-default-features
```

86 tests passent (39 unitaires, 19 intégration, 28 doc-tests).

---

## Fonctionnalités bonus

**Slab coloring** — chaque nouveau slab démarre sa zone d'objets à un offset légèrement différent (8 couleurs en rotation). Cela distribue les objets sur des positions de cache-line différentes et réduit les conflits matériels, comme Linux le fait via `cache_color` dans `mm/slub.c`.

**Statistiques** — `allocator.stats()` retourne :

```rust
Stats {
    alloc_count: usize,
    dealloc_count: usize,
    active_objects: usize,
    active_slabs: usize,
}
```

Utile pour détecter les fuites mémoire et vérifier que la récupération de slab fonctionne.

---

## Notes de conception

- **Aucune dépendance externe** — le spinlock est construit à partir de `core::sync::atomic`, pas de crate `spin`.
- **PageProvider modulaire** — `StaticPageProvider<N>` pour le bare-metal (pool en compile-time), `TestPageProvider` pour les tests hébergés.
- **Récupération de slab** — quand tous les objets d'un slab sont libérés, la page est immédiatement rendue au provider.
- **`no_std` par défaut** — la feature `std` active uniquement `TestPageProvider` pour les tests.

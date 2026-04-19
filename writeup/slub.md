# Write-up : De l'allocateur SLUB Linux à notre allocateur slab en Rust

**Angelov Onur, Slimani Anis — 4SIJ2**

Ce rapport explique comment fonctionne l'allocateur SLUB du kernel Linux et comment nous en avons implémenté une version simplifiée en Rust. On a commencé par lire `mm/slub.c` et les articles LWN, puis on a construit notre allocateur couche par couche en essayant de rester fidèle aux concepts du kernel.

---

## Table des matières

1. [Pourquoi l'allocation mémoire kernel est différente de l'espace utilisateur](#1)
2. [Le buddy allocator : le socle](#2)
3. [La nécessité d'un allocateur d'objets](#3)
4. [L'allocateur SLUB : concepts fondamentaux](#4)
5. [Structures clés dans le kernel Linux](#5)
6. [La freelist intrusive : cœur du mécanisme](#6)
7. [Fast path et slow path](#7)
8. [Problèmes d'alignement mémoire](#8)
9. [Concurrence et per-CPU caches](#9)
10. [Notre implémentation Rust — mapping avec SLUB](#10)
11. [Références](#11)

---

## 1. Pourquoi l'allocation mémoire kernel est différente de l'espace utilisateur

Dans l'espace utilisateur, `malloc`/`free` sont fournis par la libc. Ces fonctions font des appels système (`brk`, `mmap`) pour obtenir de la mémoire de l'OS, puis la découpent.

Le kernel n'a pas ce luxe — il **est** l'OS :

- Il ne peut pas faire d'appel système.
- Il doit fonctionner dans des contextes non-bloquants (interruptions, spin-locks).
- Une erreur mémoire dans le kernel est fatale (kernel panic).

| Contrainte | Détail |
|---|---|
| Pas de faute de page en IRQ | Les accès mémoire doivent être immédiats |
| Multi-cœurs | Des centaines d'allocations par seconde par CPU |
| Fragmentation | Les petits objets ne doivent pas fragmenter la RAM physique |
| Performance | L'allocation doit prendre quelques cycles (fast path) |

---

## 2. Le buddy allocator : le socle

La mémoire physique du kernel est gérée par le **buddy allocator**, qui découpe la RAM en blocs de puissances de deux (1, 2, 4… pages).

```
Mémoire physique :
┌──────┬──────┬────────────────┬────────────────────────────────┐
│  1p  │  1p  │      2p        │               4p               │
└──────┴──────┴────────────────┴────────────────────────────────┘
 order=0       order=1          order=2
```

Le buddy est bien pour les grosses zones (buffers DMA…), mais catastrophique pour les petits objets : allouer une page entière (4 KiB) pour stocker 32 octets = 99% de gaspillage. C'est pour ça que SLUB existe : il prend des pages du buddy et les redécoupe en petits objets.

---

## 3. La nécessité d'un allocateur d'objets

Le kernel crée et détruit constamment les mêmes structures : `task_struct`, `inode`, `dentry`, `sk_buff`… Chacune a une taille fixe et un cycle de vie court.

Un allocateur **slab** garde un cache d'objets pré-formatés. Quand un objet est libéré, il reste dans le cache pour être immédiatement réutilisé — on évite le round-trip vers le buddy :

```
Sans slab : alloc → buddy → initialiser → utiliser → free → buddy
Avec slab : alloc → cache → pop freelist
            free  → cache → push freelist
```

---

## 4. L'allocateur SLUB : concepts fondamentaux

SLUB est l'allocateur slab par défaut du kernel Linux depuis la version 2.6.23 (2007). Il remplace l'ancien SLAB en simplifiant la gestion des métadonnées — les infos du slab sont stockées dans la `struct page` existante, sans allocation séparée.

```
kmem_cache ("struct inode")
│
├── Slab 1 (page 0xFFFF000012340000)
│    ├── [inode 0] ALLOUÉ
│    ├── [inode 1] libre → inode 3
│    ├── [inode 2] ALLOUÉ
│    └── [inode 3] libre → NULL
│
├── Slab 2 (page 0xFFFF000012341000)
│    ├── [inode 4] libre → inode 5
│    └── [inode 5] libre → NULL
│
└── Slab 3 — PLEIN
```

Chaque **cache** correspond à un type d'objet. Chaque **slab** est une page découpée en N objets identiques. La **freelist** chaîne les objets libres entre eux.

---

## 5. Structures clés dans le kernel Linux

### `kmem_cache`

```c
struct kmem_cache {
    struct kmem_cache_cpu __percpu *cpu_slab;  // per-CPU fast path
    unsigned int size;         // taille objet + métadonnées
    unsigned int object_size;  // taille objet pure
    unsigned int offset;       // offset du pointeur freelist dans l'objet
    // ... listes de slabs partiaux, stats, ...
};
```

### `slab` / `page`

Dans SLUB, les métadonnées du slab (freelist, inuse…) sont stockées dans la `struct page` de la page physique. Pas de structure séparée à allouer — c'est l'une des simplifications clés de SLUB par rapport à l'ancien SLAB.

### Comparaison avec notre implémentation

| Linux SLUB | Notre Rust |
|---|---|
| `kmem_cache` | `Cache` (`src/cache.rs`) |
| `struct page` (champs slab) | `SlabHeader` au début de la page |
| Freelist intrusive | `FreeList` / `FreeNode` |
| per-CPU cache | Non implémenté (simplifié) |
| `kmalloc` | `SlabAllocator::alloc` |
| `kfree` | `SlabAllocator::dealloc` |

---

## 6. La freelist intrusive : cœur du mécanisme

L'idée clé : **quand un objet est libre, ses premiers octets servent de pointeur vers le prochain objet libre**. Pas de structure externe, pas d'allocation supplémentaire.

```
Page (obj_size = 32 bytes) :

Offset  0  : SlabHeader { freelist→obj2, inuse=2, capacity=4 }
Offset 64  : [obj0] ← ALLOUÉ
Offset 96  : [obj1] ← ALLOUÉ
Offset 128 : [obj2] ← LIBRE : 8 premiers bytes = ptr vers obj3
Offset 160 : [obj3] ← LIBRE : 8 premiers bytes = NULL
```

Allocation (pop) : `freelist → obj2 → obj3` devient `freelist → obj3` (obj2 retourné).
Libération (push) : obj2 remis en tête de liste.

C'est extrêmement rapide — 2-3 instructions assembleur. La contrainte pratique qu'on a découverte en codant : chaque objet doit faire au minimum 8 bytes pour pouvoir stocker le pointeur `FreeNode` quand il est libre.

---

## 7. Fast path et slow path

### Fast path — ~3 cycles

```
kmalloc(size) :
  1. Trouver le cache pour cette taille
  2. Lire cpu_slab->freelist
  3. Si freelist != NULL → pop et retourner
```

Dans notre code (`src/cache.rs`), on parcourt la liste de slabs et on pop la freelist du premier slab non-plein.

### Slow path — nouvelle page

Si tous les slabs sont pleins :

```
1. Demander une page au buddy (chez nous : PageProvider::alloc_page)
2. Initialiser le slab (découper la page en objets, construire la freelist)
3. Retourner un objet
```

### Récupération de slab

Quand le dernier objet d'un slab est libéré, on rend la page au provider :

```rust
if slab.is_empty() {
    // retirer de la liste intrusive
    unsafe { provider.dealloc_page(page) };
}
```

---

## 8. Problèmes d'alignement mémoire

L'alignement est important pour deux raisons : performance (un accès non-aligné peut coûter 2 lectures mémoire) et correction (un `u64` non-aligné ne peut pas être lu atomiquement).

Notre layout dans une page :

```
Page (4096 bytes), obj_size = 64, align = 64 :

┌───────────────────────────────────────────────────────────────┐
│ SlabHeader (~32 bytes)                                        │
├── padding jusqu'au prochain multiple de 64 ───────────────────┤
│ obj[0]  │ obj[1]  │ obj[2]  │ ...          │ obj[62]          │
└──────────────────────────────────────────────────────────────-┘
```

La fonction `align_up` arrondit à la puissance de 2 supérieure :

```rust
pub fn align_up(x: usize, a: usize) -> usize {
    (x + (a - 1)) & !(a - 1)
}
```

Exemple : `SlabHeader` finit à l'offset 32. Pour `align = 64` :
`align_up(32, 64) = 64` → le premier objet commence à l'offset 64, les 32 bytes intermédiaires sont du padding.

---

## 9. Concurrence et per-CPU caches

### Le problème

Sur un système multi-cœurs, si tous les CPUs partagent une seule freelist, chaque allocation nécessite un verrou global → contention.

### Solution SLUB : `kmem_cache_cpu`

```
CPU 0              CPU 1              CPU 2
  │                  │                  │
[cpu_slab 0]      [cpu_slab 1]      [cpu_slab 2]
[freelist locale] [freelist locale] [freelist locale]
      │                  │                  │
      └──────────────────┴──────────────────┘
                         │
                  [liste globale partielle]
                  (protégée par lock)
```

Chaque CPU a sa propre freelist locale dans `kmem_cache_cpu`. Quand elle est vide, le CPU va chercher un slab dans la liste globale (slow path avec verrou). Cela réduit la contention au strict minimum.

### Notre simplification

On n'implémente pas les caches per-CPU. Toute l'exclusion est gérée par le `Mutex` dans `LockedAllocator` :

```rust
pub struct LockedAllocator<const N: usize> {
    inner: Mutex<Option<SlabAllocator<StaticPageProvider<N>>>>,
}
```

C'est suffisant pour un OS mono-tâche ou pour comprendre les principes.

---

## 10. Notre implémentation Rust — mapping avec SLUB

### Structure du projet

```
src/
├── lib.rs            — exports publics, size classes
├── page_provider.rs  — trait PageProvider + StaticPageProvider
├── freelist.rs       — FreeList LIFO intrusive
├── slab.rs           — Slab (1 page = N objets) + align_up
├── cache.rs          — Cache par size-class + slab coloring
├── allocator.rs      — SlabAllocator (9 size classes, stats)
├── spinlock.rs       — Mutex maison (interior mutability)
└── global.rs         — LockedAllocator (impl GlobalAlloc)
```

### Flux d'une allocation

```
alloc(Layout { size: 24, align: 8 })
        │
        ▼
SlabAllocator : 24 ≤ 32 → index 2 (cache 32 bytes)
        │
        ▼
Cache::alloc()
        ├── Fast path : slab avec freelist non vide → pop
        └── Slow path : PageProvider::alloc_page() → Slab::init() → pop
```

### Fonctionnalités bonus

**Slab coloring** : chaque nouveau slab démarre à un offset légèrement différent (parmi 8 valeurs en rotation), pour distribuer les objets sur des positions de cache-line différentes — c'est ce que Linux fait avec `cache_color` dans `mm/slub.c`.

**Statistiques** : `SlabAllocator` comptabilise `alloc_count`, `dealloc_count`, `active_objects`, `active_slabs`. Utile pour vérifier qu'il n'y a pas de fuite et que la récupération de slab fonctionne.

### `GlobalAlloc` et interior mutability

`GlobalAlloc::alloc` prend `&self` (référence partagée), mais notre allocateur a besoin d'un `&mut self`. On résout ça avec notre `Mutex<T>` maison qui utilise un `AtomicBool` + `UnsafeCell<T>` pour obtenir un accès exclusif mutable depuis une référence partagée.

---

## 11. Références

- [mm/slub.c](https://elixir.bootlin.com/linux/latest/source/mm/slub.c) — implémentation SLUB
- [Documentation/mm/slub.rst](https://www.kernel.org/doc/html/latest/mm/slub.html)
- LWN : [«The SLUB allocator»](https://lwn.net/Articles/229984/) — Christoph Lameter, 2007
- [Writing an OS in Rust — Heap Allocation](https://os.phil-opp.com/heap-allocation/)
- [Writing an OS in Rust — Allocator Designs](https://os.phil-opp.com/allocator-designs/)
- [`core::alloc::GlobalAlloc`](https://doc.rust-lang.org/core/alloc/trait.GlobalAlloc.html)
- «Understanding the Linux Kernel» — Bovet & Cesati, chapitre 8

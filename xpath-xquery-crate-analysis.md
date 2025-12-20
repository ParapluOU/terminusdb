# XPath/XQuery Crate Analysis for TerminusDB Integration

## Executive Summary

**Recommendation: xrust** has a significantly more abstract and pluggable storage model compared to xee, making it the better candidate for integration with TerminusDB's custom storage layer.

---

## TerminusDB Storage Architecture

### Core Characteristics
- **Triple-based storage**: RDF model with subject-predicate-object triples
- **Immutable layers**: Versioned snapshots with delta tracking
- **Numeric ID system**: String identifiers mapped to u64 for efficiency
- **Multi-index access**: 6 index patterns (S, P, O, SP, SO, PO) for optimized queries
- **Type system**: 40+ XSD types supported via `tdb-succinct`

### Key Abstractions
```rust
Layer trait:
  - subject_id(text) -> Option<u64>
  - id_subject(u64) -> Option<String>
  - triples_s/p/o/sp(...) -> Iterator<IdTriple>
  - triple_exists(s, p, o) -> bool

Store trait:
  - create/open/delete named graphs
  - layer creation and management
```

**Location**: `/home/user/terminusdb/src/rust/terminusdb-store-prolog/`

---

## Xee Analysis

### Architecture Overview
- **XPath 3.1** implementation (almost complete)
- **XSLT 3.0** implementation (incomplete but strong foundation)
- **No XQuery support** (XQuery is a superset of XPath, not currently implemented)

### DOM/Storage Model

#### Design Philosophy
Xee uses **external dependency** for DOM storage rather than internal abstraction:
- Delegates tree handling to [`xot`](https://github.com/faassen/xot) library
- xot wraps [`indextree`](https://github.com/saschagrunert/indextree) (arena-based tree storage)

#### Storage Implementation
```
xee (XPath engine)
    ↓
xot (XML tree API wrapper)
    ↓
indextree (arena-allocated tree with Vec<Node>)
```

**indextree characteristics**:
- Arena allocation: Single `Vec` with numeric indices instead of pointers
- No `RefCell` - mutability through `&mut` access to arena
- Thread-safe, parallelizable
- Nodes referenced by `NodeId` (index into Vec)

#### Core Types
```rust
Xot           // Manages all XML tree data
Node          // Lightweight handle (essentially an index)
Value         // Enum: Element | Text | Comment | PI | Attribute | Namespace | Document
Documents     // Collection store for multiple documents
DocumentHandle // Reference to a document
```

#### Abstraction Level

**Verdict: LOW ABSTRACTION** ❌

- **No trait-based abstraction**: xot is a concrete implementation
- **Tightly coupled**: Direct dependency on indextree's arena model
- **No pluggability**: No mechanism to swap storage backends
- **API wrapping**: xot "completely wraps the indextree functionality"
- **Extension point**: Only `Itemable` trait for converting Rust types to XPath items (not storage-related)

### Integration Challenge with TerminusDB

1. **Incompatible models**: Arena allocation vs. triple store
2. **No abstraction layer**: Would require forking/rewriting xot
3. **Mismatch in navigation**: Index-based vs. query-based access
4. **No extension points**: Storage is hardcoded to indextree

---

## Xrust Analysis

### Architecture Overview
- **XPath 3.1** implementation goal (currently ~v1.0 equivalent complete)
- **XQuery 3.1** implementation goal (partial)
- **XSLT 3.0** implementation goal (currently ~v1.0 equivalent complete)

### DOM/Storage Model

#### Design Philosophy
Xrust uses **trait-based abstraction** for pluggable storage:
- Defines `Node` trait as the core abstraction
- Provides reference implementation: `smite` tree
- Engine operates against trait, not concrete implementation

#### Storage Architecture
```
XPath/XQuery/XSLT engine
    ↓
Context (reads tree structures)
    ↓
Node trait (abstraction boundary)
    ↓
smite implementation (Rc<RefCell> interior mutability)
```

#### Node Trait Definition

**27 methods total** covering:

1. **Node Creation & Identity** (3 methods)
   - `new_document()` - static constructor
   - `get_id()` - unique identifier
   - `is_same()` - identity comparison

2. **Node Properties** (5 methods)
   - `node_type()` -> NodeType enum
   - `name()` -> QualifiedName
   - `value()` -> Value
   - `is_element()`, `is_id()`, `is_idrefs()`

3. **Tree Navigation** (11 methods)
   - `child_iter()`, `first_child()` ⭐
   - `ancestor_iter()`, `parent()` ⭐
   - `descend_iter()`, `next_iter()`, `prev_iter()` ⭐
   - `attribute_iter()`, `get_attribute()`, `get_attribute_node()` ⭐
   - `owner_document()`

4. **Serialization** (4 methods)
   - `to_string()`, `to_xml()`, `to_xml_with_options()`, `to_json()`

5. **Node Creation Factory** (6 methods)
   - `new_element()`, `new_text()`, `new_attribute()`
   - `new_comment()`, `new_processing_instruction()`, `new_namespace()`

6. **Tree Modification** (5 methods)
   - `push()`, `pop()`, `insert_before()` ⭐
   - `add_attribute()`, `add_namespace()`

7. **Copying & Comparison** (4 methods)
   - `shallow_copy()`, `deep_copy()`
   - `get_canonical()`, `eq()`

8. **Document Management** (5 methods)
   - `xmldecl()`, `set_xmldecl()`
   - `get_dtd()`, `set_dtd()`, `validate()`

#### Trait Characteristics

**Required vs. Provided**:
- **Required**: ~20 methods (core functionality)
- **Provided**: 7 methods with default implementations
  - `first_child()` - delegates to `child_iter()`
  - `parent()` - delegates to `ancestor_iter()`
  - `is_element()` - simple wrapper
  - `eq()` - sophisticated recursive tree comparison
  - `to_json()` - returns empty String

**Associated Types**:
```rust
type NodeIterator: Iterator<Item = Self>
```

**Type Bounds**:
```rust
Self: Clone + PartialEq + Debug
```

#### Abstraction Level

**Verdict: HIGH ABSTRACTION** ✅

- **Trait-based design**: Clean separation between interface and implementation
- **Pluggable backends**: Any type implementing `Node` trait works
- **Engine independence**: XPath/XQuery engine uses trait methods only
- **Reference implementation**: smite serves as example, not requirement
- **WASM compatibility goal**: "Must not have dependencies on file and network I/O" - drives abstraction

### Current Implementation: smite

#### Internal Structure
```rust
RNode = Rc<Node>  // Reference-counted node

Node {
  inner: RefCell<NodeInner>
}

NodeInner = enum {
  Document { xmldecl, children, unattached, dtd }
  Element { name, attributes: BTreeMap, children, namespaces }
  Text { parent: Weak<Node>, content }
  Comment { parent: Weak<Node>, content }
  Attribute { parent: Weak<Node>, name, value }
  // ... etc
}
```

#### Key Patterns
- **Interior mutability**: `RefCell` allows mutation through shared references
- **Cycle prevention**: `Weak<Node>` for parent pointers
- **Reference counting**: `Rc` for cheap cloning and shared ownership
- **Index-based siblings**: Position stored as index in parent's child vector

### Integration Potential with TerminusDB

#### Viable Mapping Strategy

The Node trait could be implemented on top of TerminusDB's triple store:

1. **Node representation**:
   - Map XML nodes to RDF triples with custom predicates
   - Use TerminusDB's u64 IDs as node identifiers
   - Store node type, name, value as triple patterns

2. **Navigation mapping**:
   ```
   child_iter() → query triples_p(HAS_CHILD_PREDICATE)
   parent() → query triples_o(current_node_id) with PARENT_PREDICATE
   attribute_iter() → query triples_s(node_id) filtering ATTRIBUTE_TYPE
   ```

3. **Multi-index leverage**:
   - Use TerminusDB's SP index for efficient parent->children queries
   - Use O index for child->parent lookups
   - Use S index for node->attributes queries

4. **Layer benefits**:
   - Immutable snapshots provide versioning for XSLT transformations
   - Delta layers track document modifications
   - Multi-version support for transactional XQuery updates

#### Implementation Challenges

1. **Comprehensive trait**: 27 methods require substantial implementation
2. **Serialization**: `to_xml()`, `to_json()` need custom logic
3. **Document structure**: DTD, XML declarations need triple encoding
4. **Validation**: Schema validation against TerminusDB's schema layer
5. **Performance**: Iterator overhead vs. native arena access

#### Advantages

1. **Clean abstraction boundary**: Trait provides clear integration point
2. **No forking required**: Implement trait, use xrust as-is
3. **Testable**: Can validate against smite reference implementation
4. **Future-proof**: Updates to xrust engine don't break custom backend

---

## Detailed Comparison Matrix

| Aspect | Xee | Xrust |
|--------|-----|-------|
| **Storage Abstraction** | Concrete (indextree) | Trait-based (Node) |
| **Pluggability** | None | High (trait implementation) |
| **Current Backend** | xot → indextree | smite (Rc/RefCell) |
| **Integration Point** | None available | Node trait (27 methods) |
| **XPath Support** | 3.1 (almost complete) | 1.0 equivalent (complete) |
| **XQuery Support** | None | Partial (goal: 3.1) |
| **XSLT Support** | 3.0 (incomplete) | 1.0 equivalent (complete) |
| **Storage Model** | Arena Vec | Flexible (trait) |
| **Mutability** | &mut arena access | Interior mutability pattern |
| **Thread Safety** | Yes (Vec-based) | Depends on impl |
| **Parent Navigation** | Yes (via xot/indextree) | Yes (trait method) |
| **Fork Required** | Yes (xot rewrite) | No (trait impl) |
| **Reference Counting** | No (indices) | Implementation choice |
| **Memory Model** | Contiguous Vec | Implementation choice |
| **Extension Mechanism** | Itemable (values only) | Node trait (storage) |
| **Maturity** | Higher (XPath 3.1) | Lower (XPath 1.0) |
| **API Complexity** | Simpler (fewer methods) | More complex (27 methods) |

---

## Integration Recommendations

### Primary Recommendation: Xrust

**Reasons**:
1. ✅ **Trait abstraction** provides clean integration boundary
2. ✅ **No fork required** - implement Node trait, use xrust engine
3. ✅ **XQuery support** - critical for database query use cases
4. ✅ **Flexible storage** - trait doesn't mandate implementation details
5. ✅ **Testable** - can validate against reference implementation

**Trade-offs**:
- ⚠️ Less mature XPath support (1.0 vs 3.1)
- ⚠️ 27 methods to implement (substantial effort)
- ⚠️ Performance unknowns with triple-store backend

### Alternative Path: Xee (Not Recommended)

Would require:
1. ❌ Fork xot library to abstract indextree dependency
2. ❌ Create trait abstraction for tree storage
3. ❌ Implement trait on TerminusDB storage
4. ❌ Maintain fork as xot/xee evolves
5. ❌ No XQuery support

**Only viable if**: XPath 3.1 completeness is critical AND willing to maintain forks

---

## Implementation Roadmap (Xrust)

### Phase 1: Proof of Concept
1. Implement minimal Node trait subset (10 core methods)
2. Map basic XML structure to RDF triples
3. Test simple XPath queries (//element, /root/child)
4. Validate performance with small documents

### Phase 2: Complete Navigation
1. Implement all 11 navigation methods
2. Optimize with TerminusDB's multi-index patterns
3. Add attribute and namespace handling
4. Test complex XPath axes (ancestor, descendant, following-sibling)

### Phase 3: Modification Support
1. Implement tree modification methods (push, pop, insert_before)
2. Map to TerminusDB layer builders
3. Test XSLT transformations
4. Validate transactional semantics

### Phase 4: Full Compliance
1. Implement serialization methods
2. Add document management (DTD, XML declarations)
3. Complete remaining Node trait methods
4. Performance optimization and benchmarking

### Phase 5: Production Hardening
1. Error handling and edge cases
2. Memory optimization
3. Concurrent access patterns
4. Integration testing with real-world XQuery/XSLT

---

## Technical Deep Dive: Mapping Strategies

### XML Node → RDF Triple Mapping

#### Element Node
```
Subject: node_<id>
Predicates:
  - rdf:type → xdom:Element
  - xdom:elementName → "elementName"
  - xdom:hasChild → node_<child_id> (multiple)
  - xdom:hasAttribute → attr_<id> (multiple)
  - xdom:parentNode → node_<parent_id>
```

#### Text Node
```
Subject: node_<id>
Predicates:
  - rdf:type → xdom:Text
  - xdom:textContent → "actual text value"
  - xdom:parentNode → node_<parent_id>
```

#### Attribute Node
```
Subject: attr_<id>
Predicates:
  - rdf:type → xdom:Attribute
  - xdom:attributeName → "attrName"
  - xdom:attributeValue → "attrValue"
  - xdom:ownerElement → node_<element_id>
```

### Query Translation Examples

#### XPath: `//book[@genre='fiction']/title`

**Step 1**: `//book` (descendant-or-self::book)
```rust
// Query all nodes with type Element and name "book"
layer.triples_p(ELEMENT_NAME_PREDICATE)
     .filter(|t| layer.id_object(t.object) == Some("book"))
     .map(|t| t.subject)
```

**Step 2**: `[@genre='fiction']` (predicate filter)
```rust
// For each book node, check if it has attribute genre='fiction'
candidates.filter(|&node_id| {
    // Find attributes of this node
    layer.triples_sp(node_id, HAS_ATTRIBUTE_PREDICATE)
         .any(|attr_triple| {
             // Check if attribute name is "genre" and value is "fiction"
             check_attribute(layer, attr_triple.object, "genre", "fiction")
         })
})
```

**Step 3**: `/title` (child axis)
```rust
// Get children of filtered nodes with name "title"
filtered_books.flat_map(|book_id| {
    layer.triples_sp(book_id, HAS_CHILD_PREDICATE)
         .filter(|t| {
             get_element_name(layer, t.object) == Some("title")
         })
         .map(|t| t.object)
})
```

### Iterator Implementation Pattern

```rust
struct TerminusNodeChildIterator {
    layer: SyncStoreLayer,
    parent_id: u64,
    inner: Box<dyn Iterator<Item = IdTriple>>,
}

impl Iterator for TerminusNodeChildIterator {
    type Item = TerminusNode;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|triple| {
            TerminusNode::from_id(self.layer.clone(), triple.object)
        })
    }
}

// In Node trait implementation:
fn child_iter(&self) -> Self::NodeIterator {
    let iter = self.layer.triples_sp(self.id, HAS_CHILD_PREDICATE);
    TerminusNodeChildIterator {
        layer: self.layer.clone(),
        parent_id: self.id,
        inner: Box::new(iter),
    }
}
```

---

## Performance Considerations

### TerminusDB Strengths
- ✅ Multi-index access (optimal for different query patterns)
- ✅ Immutable layers (snapshot isolation, no locking)
- ✅ Numeric IDs (efficient comparison and storage)
- ✅ Optimized triple iteration

### Potential Bottlenecks
- ⚠️ Triple overhead (3 values per relationship vs. direct pointers)
- ⚠️ Iterator allocation (boxing for trait objects)
- ⚠️ String conversions (ID ↔ text mapping)
- ⚠️ Predicate filtering (vs. direct child vector access)

### Optimization Strategies
1. **Caching**: Store frequently accessed mappings (ID → name)
2. **Batching**: Prefetch related triples in single query
3. **Index selection**: Use query planner for optimal index
4. **Lazy evaluation**: Minimize iterator materialization
5. **Custom predicates**: Define specialized predicates for XPath patterns

---

## Conclusion

**Xrust is the clear choice** for TerminusDB integration due to its trait-based abstraction model. While it requires implementing 27 trait methods, this provides a clean architectural boundary without requiring forks or upstream modifications.

The Node trait abstraction enables:
- ✅ Integration with TerminusDB's triple store
- ✅ Leverage of existing multi-index infrastructure
- ✅ XQuery support for database queries
- ✅ XSLT support for document transformation
- ✅ Future extensibility

Xee's tight coupling to indextree makes it unsuitable for custom storage backends without significant refactoring of the xot library.

---

## References

### Xee
- [Xee GitHub](https://github.com/Paligo/xee)
- [Xee Blog Post: Modern XPath and XSLT Engine in Rust](https://blog.startifact.com/posts/xee/)
- [xee_xpath Documentation](https://docs.rs/xee-xpath/latest/xee_xpath/)
- [xot GitHub](https://github.com/faassen/xot)
- [xot Documentation](https://docs.rs/xot/latest/xot/)

### Xrust
- [xrust GitHub](https://github.com/ballsteve/xrust)
- [xrust Documentation](https://docs.rs/xrust/)
- [xrust Website](https://ballsteve.github.io/xrust/)
- [xrust Crates.io](https://crates.io/crates/xrust)

### Supporting Libraries
- [indextree GitHub](https://github.com/saschagrunert/indextree)
- [indextree Documentation](https://docs.rs/indextree/latest/indextree/)
- [generational-indextree](https://crates.io/crates/generational-indextree)

### TerminusDB
- Storage: `/home/user/terminusdb/src/rust/terminusdb-store-prolog/`
- Key files: `store.rs`, `layer.rs`, `builder.rs`, `value.rs`, `cache.rs`

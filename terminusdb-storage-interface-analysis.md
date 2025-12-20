# TerminusDB Storage Interface Analysis

## Summary: Two-Level Architecture

TerminusDB provides **both** triple-level and document-level interfaces:

1. **Triple-level**: Low-level `Layer` trait from `terminus-store` (v0.21.5)
2. **Document-level**: High-level `DocumentContext` from `terminusdb-community`

## Level 1: Triple-Level Interface (terminus-store Layer)

### Core Trait: `Layer`

Located in: `terminus-store` crate v0.21.5 (external dependency)

### ID Mapping Methods

**String ↔ Numeric ID conversion**:

```rust
// Subject ID methods
fn subject_id(&self, subject: &str) -> Option<u64>
fn id_subject(&self, id: u64) -> Option<String>

// Predicate ID methods
fn predicate_id(&self, predicate: &str) -> Option<u64>
fn id_predicate(&self, id: u64) -> Option<String>

// Object ID methods (handles both nodes and values)
fn object_node_id(&self, node: &str) -> Option<u64>
fn object_value_id(&self, value: &TypedDictEntry) -> Option<u64>
fn id_object(&self, id: u64) -> Option<ObjectType>  // ObjectType::Node(String) | ObjectType::Value(TypedDictEntry)
```

**Why numeric IDs?**
- Efficient storage and comparison
- Small memory footprint (u64 vs String)
- Fast index lookups

### Triple Iteration Methods

**Multi-index access patterns**:

```rust
// All triples
fn triples(&self) -> impl Iterator<Item = IdTriple>

// Single-index queries
fn triples_s(&self, subject: u64) -> impl Iterator<Item = IdTriple>     // Subject index
fn triples_p(&self, predicate: u64) -> impl Iterator<Item = IdTriple>   // Predicate index
fn triples_o(&self, object: u64) -> impl Iterator<Item = IdTriple>      // Object index

// Two-index queries (most efficient)
fn triples_sp(&self, subject: u64, predicate: u64) -> impl Iterator<Item = IdTriple>  // Subject-Predicate

// Existence check (fastest)
fn triple_exists(&self, subject: u64, predicate: u64, object: u64) -> bool
```

**IdTriple structure**:
```rust
pub struct IdTriple {
    pub subject: u64,
    pub predicate: u64,
    pub object: u64,
}
```

### Delta Layer Methods

**For tracking changes between versions**:

```rust
// Additions in this layer (compared to parent)
fn triple_additions(&self) -> impl Iterator<Item = IdTriple>
fn triple_additions_s(&self, subject: u64) -> impl Iterator<Item = IdTriple>
fn triple_additions_p(&self, predicate: u64) -> impl Iterator<Item = IdTriple>
fn triple_additions_o(&self, object: u64) -> impl Iterator<Item = IdTriple>
fn triple_additions_sp(&self, subject: u64, predicate: u64) -> impl Iterator<Item = IdTriple>
fn triple_addition_exists(&self, s: u64, p: u64, o: u64) -> bool

// Removals in this layer
fn triple_removals(&self) -> impl Iterator<Item = IdTriple>
fn triple_removals_s(&self, subject: u64) -> impl Iterator<Item = IdTriple>
// ... etc (same patterns as additions)

// Counts
fn triple_addition_count(&self) -> usize
fn triple_removal_count(&self) -> usize
```

### Layer Hierarchy Methods

**Versioning and consolidation**:

```rust
// Parent layer
fn parent(&self) -> Option<Layer>

// Consolidation operations
fn squash(&self) -> Result<Layer>                    // Merge with parent
fn squash_upto(&self, layer: &Layer) -> Result<Layer>  // Merge to specific ancestor
fn rollup(&self) -> Result<()>                       // Optimize storage
fn rollup_upto(&self, layer: &Layer) -> Result<()>
fn imprecise_rollup_upto(&self, layer: &Layer) -> Result<()>
```

### Statistics Methods

```rust
fn node_and_value_count(&self) -> usize   // Total unique subjects + objects
fn predicate_count(&self) -> usize         // Total unique predicates
```

### Usage Example from TerminusDB Source

From `/home/user/terminusdb/src/rust/terminusdb-store-prolog/src/layer.rs:237-268`:

```rust
// Intelligent query planner based on which IDs are bound
let iter: Box<dyn Iterator<Item=IdTriple>>;

if let Some(subject_id) = subject_id_term.get::<u64>() {
    if let Some(predicate_id) = predicate_id_term.get::<u64>() {
        if let Some(object_id) = object_id_term.get::<u64>() {
            // All bound: check existence
            if layer.triple_exists(subject_id, predicate_id, object_id) {
                return Ok(None);  // Found it
            } else {
                return Err(Failure);  // Doesn't exist
            }
        } else {
            // S and P bound: use SP index (most efficient)
            iter = layer.triples_sp(subject_id, predicate_id);
        }
    } else {
        // Only S bound: use S index
        iter = layer.triples_s(subject_id);
    }
} else if let Some(predicate_id) = predicate_id_term.get::<u64>() {
    // Only P bound: use P index
    iter = layer.triples_p(predicate_id);
} else {
    // Nothing bound: scan everything
    iter = layer.triples();
}
```

**Key insight**: Query planner selects optimal index based on bound variables!

## Level 2: Document-Level Interface (terminusdb-community)

### DocumentContext Structure

Located in: `/home/user/terminusdb/src/rust/terminusdb-community/src/doc/mod.rs`

```rust
pub struct DocumentContext<L: Layer + Clone> {
    schema: Option<L>,              // Schema layer
    layer: Option<L>,               // Instance data layer
    prefixes: PrefixContracter,     // URI prefix compression

    // Schema-derived metadata (cached)
    types: HashSet<u64>,            // All type IDs
    subtypes: HashMap<String, HashSet<u64>>,  // Subtype relationships
    document_types: HashSet<u64>,   // Document type IDs
    unfoldables: HashSet<u64>,      // Unfoldable type IDs
    enums: HashMap<u64, String>,    // Enum values
    set_pairs: HashSet<(u64, u64)>, // Set type-predicate pairs
    value_hashes: HashSet<u64>,     // Value hash type IDs

    // System identifiers
    rdf: Arc<RdfIds<L>>,           // RDF predicates (rdf:type, rdf:first, etc.)
    sys: Arc<SysIds<L>>,           // System predicates (sys:class, sys:inherits, etc.)
}
```

### How It Works

**Initialization** (from `doc/mod.rs:46-150`):

```rust
impl DocumentContext {
    pub fn new(schema: L, instance: Option<L>) -> Self {
        // 1. Query schema for type definitions
        let document_types = schema_query_context
            .get_document_type_ids_from_schema()
            .collect();

        // 2. Build inheritance graph from triples
        for triple in schema.triples_p(inherits_id) {
            inheritance_graph.insert(triple.subject, triple.object);
        }

        // 3. Translate schema IDs to instance IDs
        for schema_type_id in schema_type_ids {
            if let Some(instance_id) = translate_id(schema_type_id) {
                types.insert(instance_id);
            }
        }

        // Creates cached metadata for fast document operations
    }
}
```

### SchemaQueryContext

Located in: `/home/user/terminusdb/src/rust/terminusdb-community/src/schema.rs`

**Builds high-level graph structures from triples**:

```rust
impl SchemaQueryContext {
    // Build inheritance graph (from schema.rs:28-40)
    fn get_inheritance_graph(&self) -> InheritanceGraph {
        let mut graph = HashMap::new();

        // Query all sys:inherits triples
        for triple in self.layer.triples_p(inherits_id) {
            graph.entry(triple.subject)
                .or_insert_with(HashSet::new)
                .insert(triple.object);
        }

        InheritanceGraph(graph)
    }

    // Get all document types (from schema.rs:145-150)
    fn get_document_type_ids_from_schema(&self) -> impl Iterator<Item = u64> {
        self.get_type_ids_from_schema()
            .filter(|t| !subdocument_ids.contains(t))
    }

    // Get Set types (from schema.rs:104-143)
    fn get_set_pairs_from_schema(&self) -> impl Iterator<Item = (u64, u64)> {
        // Finds (type_id, predicate_id) pairs where predicate has sys:Set range
        self.layer.triples_o(sys_set_id)
            .filter(|t| t.predicate == rdf_type_id)
            .flat_map(|t| {
                // Navigate from Set type back to predicates that use it
                // Returns (class_id, predicate_id) pairs
            })
    }
}
```

**Document-level abstractions built on triples**:
- Types and subtypes
- Document vs subdocument classification
- Enum definitions
- Set predicates
- Unfoldable types

## Integration Comparison

### Triple-Level Integration

**Direct `Layer` trait usage**:

```rust
// Example: Find all elements named "book"
let element_name_pred = layer.predicate_id("xdom:elementName")?;
let book_name = layer.object_value_id("book")?;

for triple in layer.triples_p(element_name_pred) {
    if triple.object == book_name {
        let element_id = triple.subject;
        // Found a <book> element

        // Get children
        let has_child_pred = layer.predicate_id("xdom:hasChild")?;
        for child_triple in layer.triples_sp(element_id, has_child_pred) {
            let child_id = child_triple.object;
            // Process child
        }
    }
}
```

**Pros**:
- ✅ Maximum performance (direct index access)
- ✅ Full control over query optimization
- ✅ Can leverage all 6 index patterns
- ✅ No abstraction overhead

**Cons**:
- ⚠️ Verbose (manual ID lookups)
- ⚠️ Must manage predicate IDs
- ⚠️ No schema awareness
- ⚠️ More code to write

### Document-Level Integration

**Using `DocumentContext`**:

```rust
// Example: Query documents by type
let doc_context = DocumentContext::new(schema_layer, Some(instance_layer));

// DocumentContext already has:
// - All document type IDs cached
// - Inheritance graph precomputed
// - Enum mappings ready
// - Prefix compression setup

// Iterate documents
for doc_type_id in &doc_context.document_types {
    // Find all instances of this document type
    let rdf_type_pred = doc_context.rdf.type_();

    for triple in doc_context.layer.triples_o(doc_type_id) {
        if triple.predicate == rdf_type_pred {
            let document_id = triple.subject;
            // Process document
        }
    }
}
```

**Pros**:
- ✅ Schema-aware (types, inheritance, enums)
- ✅ Cached metadata (faster repeated queries)
- ✅ Higher-level abstractions
- ✅ Prefix management built-in

**Cons**:
- ⚠️ Still operates on triples
- ⚠️ Initialization overhead
- ⚠️ Memory for cached structures
- ⚠️ Not a full document abstraction (no "get document by ID")

## Key Observations

### 1. **Purely Triple-Based**

TerminusDB storage has **no document-level retrieval methods** like:
- ❌ `get_document(id)` → returns whole document
- ❌ `query_documents(type)` → returns document collection
- ❌ `get_document_property(doc_id, property)` → returns value

**Everything is triples**:
```
Subject  Predicate         Object
doc:1    rdf:type          Book
doc:1    book:title        "XPath Guide"
doc:1    book:author       author:42
author:42 author:name      "John Smith"
```

### 2. **Schema Provides Structure**

The `DocumentContext` interprets triples as documents using schema:

```
Schema layer defines:
  Book a sys:Class
  Book sys:subdocument Author
  book:title a owl:DatatypeProperty

Instance layer contains:
  doc:1 rdf:type Book
  doc:1 book:title "XPath Guide"

→ DocumentContext recognizes doc:1 as a Book document
```

### 3. **Multi-Index Query Optimization**

TerminusDB provides **6 access patterns**:

| Pattern | Method | Use Case | Example XPath |
|---------|--------|----------|---------------|
| All | `triples()` | Scan everything | `//node()` |
| S | `triples_s(s)` | Get all properties of node | `$node/*` |
| P | `triples_p(p)` | All nodes with property | `//@href` |
| O | `triples_o(o)` | Reverse lookup | Parent of node |
| SP | `triples_sp(s,p)` | Specific property of node | `$node/title` |
| Exists | `triple_exists(s,p,o)` | Test existence | `$node[@type='book']` |

**Missing**: SO, PO, SPO indexes (would need filtering)

### 4. **Immutable Layers with Deltas**

Each layer is immutable - changes tracked as additions/removals:

```rust
// Layer N+1 on top of Layer N
layer_n1.triple_additions()  // New triples
layer_n1.triple_removals()   // Deleted triples

// Consolidated view (all effective triples)
layer_n1.triples()           // Additions + (Parent triples - Removals)
```

**For XPath/XQuery**: Can expose different versions as different document collections!

## XPath/XQuery Integration Strategies

### Strategy 1: Direct Triple Mapping (Low-Level)

**Implement xrust `Node` trait on triple storage**:

```rust
struct TerminusNode {
    layer: SyncStoreLayer,
    id: u64,
}

impl Node for TerminusNode {
    fn child_iter(&self) -> impl Iterator<Item = Self> {
        // Use SP index
        let has_child_pred = self.layer.predicate_id("xdom:hasChild").unwrap();
        self.layer.triples_sp(self.id, has_child_pred)
            .map(|t| TerminusNode { layer: self.layer.clone(), id: t.object })
    }

    fn parent(&self) -> Option<Self> {
        // Use O index (reverse lookup)
        let has_child_pred = self.layer.predicate_id("xdom:hasChild").unwrap();
        self.layer.triples_o(self.id)
            .find(|t| t.predicate == has_child_pred)
            .map(|t| TerminusNode { layer: self.layer.clone(), id: t.subject })
    }

    fn name(&self) -> QualifiedName {
        // Use S index to find name
        let name_pred = self.layer.predicate_id("xdom:elementName").unwrap();
        let name_str = self.layer.triples_sp(self.id, name_pred)
            .next()
            .and_then(|t| self.layer.id_object(t.object))
            .unwrap();
        // ... convert to QualifiedName
    }
}
```

**Index usage**:
- `child_iter()` → SP index
- `parent()` → O index with predicate filter
- `name()` → SP index
- `attribute_iter()` → SP index with different predicate

**Performance**: Good - uses native indexes

### Strategy 2: DocumentContext-Enhanced (High-Level)

**Leverage schema awareness**:

```rust
struct SchemaAwareNode {
    layer: SyncStoreLayer,
    doc_context: Arc<DocumentContext>,
    id: u64,
}

impl Node for SchemaAwareNode {
    fn is_element(&self) -> bool {
        // Use cached type information
        let rdf_type_pred = self.doc_context.rdf.type_();
        self.layer.triples_s(self.id)
            .any(|t| t.predicate == rdf_type_pred &&
                     self.doc_context.types.contains(&t.object))
    }

    // Can use inheritance_graph for type checking
    // Can use enum mappings for attribute values
    // Can use prefix contracter for namespaces
}
```

**Advantages**:
- Type-aware navigation
- Inheritance support
- Enum handling
- Better namespace support

### Strategy 3: Hybrid with Materialization Cache

**Best of both worlds**:

```rust
struct CachedTerminusNode {
    layer: SyncStoreLayer,
    id: u64,
    cached_children: OnceCell<Vec<u64>>,  // Lazy cache
    cached_name: OnceCell<String>,
}

impl Node for CachedTerminusNode {
    fn child_iter(&self) -> impl Iterator {
        // First access: query and cache
        self.cached_children.get_or_init(|| {
            let pred = self.layer.predicate_id("xdom:hasChild").unwrap();
            self.layer.triples_sp(self.id, pred)
                .map(|t| t.object)
                .collect()
        }).iter().map(|&id| CachedTerminusNode::new(self.layer.clone(), id))
    }
}
```

**Optimizes hot paths** while keeping storage lightweight.

## Recommendation for XPath/XQuery

### Use **Strategy 1 (Direct Triple Mapping)** for initial implementation

**Why**:
1. ✅ **Simplest** - no schema complexity
2. ✅ **Fastest** - direct index usage
3. ✅ **Complete control** - can optimize per query pattern
4. ✅ **Works with any RDF** - not tied to TerminusDB schema conventions

**Then add Strategy 2 features incrementally**:
- Schema-aware type checking
- Inheritance graph queries
- Enum mappings
- Advanced namespace handling

### Predicate Design for XML Storage

**Suggested predicates**:
```turtle
xdom:elementName      # Element name
xdom:textContent      # Text node content
xdom:attributeName    # Attribute name
xdom:attributeValue   # Attribute value
xdom:hasChild         # Parent-child relationship
xdom:nextSibling      # Sibling order
xdom:namespace        # Namespace URI
xdom:prefix           # Namespace prefix
xdom:nodeType         # Node type (element=1, text=3, etc.)
```

**Example document encoding**:
```xml
<book title="XPath Guide">
  <author>John Smith</author>
</book>
```

**As triples**:
```turtle
:n1 xdom:nodeType 1 .
:n1 xdom:elementName "book" .
:n1 xdom:hasChild :n2 .
:n1 xdom:attributeName "title" .
:n1 xdom:attributeValue "XPath Guide" .

:n2 xdom:nodeType 1 .
:n2 xdom:elementName "author" .
:n2 xdom:hasChild :n3 .

:n3 xdom:nodeType 3 .
:n3 xdom:textContent "John Smith" .
```

**Queries use optimal indexes**:
- Find element by name: `triples_p(elementName) + filter`
- Get children: `triples_sp(node_id, hasChild)`
- Get attributes: `triples_sp(node_id, attributeName)`
- Get parent: `triples_o(node_id) where p = hasChild`

## Summary

### TerminusDB Storage IS:
- ✅ **Pure triple store** with excellent indexing
- ✅ **Multi-index** (S, P, O, SP patterns)
- ✅ **Versioned** (immutable layers + deltas)
- ✅ **Optimized** (numeric IDs, efficient iteration)

### TerminusDB Storage IS NOT:
- ❌ **Document database** (no get_document_by_id)
- ❌ **Object store** (no object serialization)
- ❌ **Hierarchical database** (no native tree navigation)

### For XPath/XQuery Integration:

**Triple-level is sufficient and optimal**:
- Implement `Node` trait using `Layer` methods
- Map XPath axes to triple queries
- Use SP index for children, O index for parent
- Leverage P index for element/attribute searches
- **Performance will be good** with proper index selection

**DocumentContext is optional enhancement**:
- Useful if exposing TerminusDB documents to XPath
- Schema-aware type checking
- Not needed for pure XML storage

**Bottom line**: TerminusDB's triple interface is **perfect for XPath/XQuery** - just need to encode XML as triples with appropriate predicates and implement navigation methods using the multi-index system! 🎯

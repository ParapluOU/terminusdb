# XQuery Performance Analysis: Collection & Database Model

## Your Concern: Performance with Discrete Documents ⚠️

**You're absolutely right** - if XQuery has to parse each document from disk separately for every query, performance would be terrible for:
- Joins across documents
- Aggregations over collections
- Path expressions spanning multiple docs
- Any `collection()` or `fn:doc()` queries

## The Reality: Both Systems Have the SAME Model

### Xrust: Closure-Based External Resolution

**Discovery**: Xrust doesn't have built-in collection/database management either!

```rust
// From xrust/src/transform/context.rs:568-577
pub struct StaticContext<N: Node, F, G, H>
where
    F: FnMut(&str) -> Result<(), Error>,               // Message callback
    G: FnMut(&str) -> Result<N, Error>,                // Parse string → tree
    H: FnMut(&Url) -> Result<String, Error>,           // Fetch URL → string
{
    pub(crate) message: Option<F>,
    pub(crate) parser: Option<G>,
    pub(crate) fetcher: Option<H>,  // ← THIS is how fn:doc() works
}
```

**How it works**:
1. XQuery calls `fn:doc("http://example.com/doc.xml")`
2. Xrust calls the `fetcher` closure with the URL
3. **Application** provides the closure - decides how to fetch/cache
4. Fetcher returns string, `parser` closure converts to tree
5. No built-in caching, indexing, or optimization

**From the docs** (xrust/src/lib.rs:46-48):
> "One aim of the library is to be usable in a WASM environment. To allow that, the library must not have dependencies on file and network I/O, since that is provided by the host browser environment. **Where external resources, i.e. URLs, are required the application must provide a closure.**"

### Xee: Same Model - No Built-in Collections

**Current state**: Xee doesn't have XQuery yet, but uses similar pattern:

```rust
// From xee-interpreter/src/xml/document.rs
pub struct Documents {
    documents: Vec<Document>,
    by_uri: HashMap<IriString, DocumentHandle>,
}

pub fn add_string(&mut self, xot: &mut Xot, uri: Option<&IriStr>, xml: &str)
    -> Result<DocumentHandle, DocumentsError> {
    let root = xot.parse(xml)?;  // Parse entire string
    self.add_root(uri, root)
}
```

**No built-in**:
- ❌ Lazy loading
- ❌ Persistent storage
- ❌ Query optimization across documents
- ❌ Indexing

## Production XQuery Databases: How They Actually Work

### BaseX Architecture

From [BaseX Documentation](https://docs.basex.org/main/Databases):

```
Database Storage:
- Pre-parsed XML stored in optimized binary format
- Multiple index structures (names, paths, full-text, attributes)
- Main-memory option (MAINMEM) for small datasets
- On-disk persistent storage with caching
```

**Key features**:
- Documents parsed ONCE at import time
- Stored in proprietary binary format (not raw XML)
- Multiple index types enable fast queries:
  - **Name index**: All element/attribute names
  - **Path index**: Full path expressions
  - **Value index**: Text content
  - **Full-text index**: Text search
  - **Attribute index**: Attribute values

**Query execution**:
```xquery
collection("mydb")//book[@price < 10]
```
→ Uses indexes to find matching books without parsing XML
→ Path index for `//book`, value index for `@price < 10`

### Oracle XML DB

From [Oracle XML DB Documentation](https://docs.oracle.com/en/database/oracle/oracle-database/21/adxdb/xquery-and-XML-DB.html):

**Storage models**:
1. **Binary XML** - Optimized binary format with token compression
2. **Structured storage** - Shredded into relational tables
3. **Unstructured storage** - CLOB with indexes

**Collection mapping**:
```xquery
fn:collection("oradb:/EMPLOYEES/EMP")
```
→ Maps to relational table `EMPLOYEES.EMP`
→ No XML parsing - generates XML from relational data on-the-fly
→ Uses SQL optimizer for joins/filters

### eXist-db Architecture

**Storage**:
- B+-tree based native XML database
- Documents indexed at element/attribute level
- Structural indexes for fast navigation
- Range indexes for value comparisons

**Query optimization**:
- Query rewriter uses indexes
- Cost-based optimization
- Lazy evaluation with streaming

## The Performance Gap

### Naive Implementation (Current xrust/xee model)

```
Query: collection("docs")//book[@genre='fiction']/title

For each document in "docs":
  1. fetcher("doc1.xml") → fetch from disk/network
  2. parser(xml_string) → parse entire document into tree
  3. Traverse tree with XPath //book[@genre='fiction']/title
  4. Collect results
  Repeat for doc2, doc3, ... doc1000

Time complexity: O(n * parse_time + n * traverse_time)
For 1000 docs: ~10-30 seconds
```

### Production Database (BaseX/Oracle)

```
Query: collection("docs")//book[@genre='fiction']/title

1. Consult path index for //book
2. Consult attribute index for @genre='fiction'
3. Intersect result sets
4. Project to /title
5. Return results

Time complexity: O(log n * index_lookups)
For 1000 docs: ~10-100 milliseconds
```

**Performance difference**: **100-1000x faster** 🚀

## Solutions for TerminusDB Integration

### Option 1: Smart Closure Implementation (Quick Fix)

**Idea**: Implement intelligent `fetcher` and `parser` closures that use TerminusDB as cache:

```rust
// For xrust integration
let fetcher = |url: &Url| -> Result<String, Error> {
    // Check if document already in TerminusDB
    if let Some(cached) = terminusdb.get_document(url)? {
        // Serialize from TerminusDB triples back to XML
        return Ok(serialize_to_xml(cached)?);
    }

    // Otherwise fetch from network
    let xml = http_get(url)?;

    // Cache in TerminusDB for next time
    terminusdb.store_document(url, &xml)?;

    Ok(xml)
};

let parser = |xml: &str| -> Result<RNode, Error> {
    // Parse into xrust tree
    xrust::parser::xml::parse(xml)
};
```

**Pros**:
- ✅ Simple to implement
- ✅ Reuses parsed documents
- ✅ Works with existing xrust API

**Cons**:
- ⚠️ Still parses XML every time (TerminusDB → XML → xrust tree)
- ⚠️ No query optimization (full tree traversal)
- ⚠️ Memory overhead (TerminusDB + xrust trees)
- 📊 **Performance**: ~10-50x faster than naive, still 10-100x slower than native DB

### Option 2: Deep Integration (Abstract the Tree)

**Idea**: Implement xrust's `Node` trait directly on TerminusDB storage:

```rust
impl Node for TerminusNode {
    fn child_iter(&self) -> Self::NodeIterator {
        // Query TerminusDB: triples_sp(node_id, HAS_CHILD)
        // Uses SP index - fast
        self.layer.triples_sp(self.id, HAS_CHILD_PREDICATE)
            .map(|t| TerminusNode::new(self.layer.clone(), t.object))
    }

    // ... etc
}
```

**With indexes for common XPath patterns**:
```rust
// Pre-computed indexes in TerminusDB
ELEMENT_NAME_INDEX: P index on ELEMENT_NAME predicate
ATTRIBUTE_INDEX: P index on HAS_ATTRIBUTE predicate
CHILD_INDEX: P index on HAS_CHILD predicate
TEXT_CONTENT_INDEX: O index for text values
```

**Collection queries**:
```rust
// fn:collection("books") implementation
let fetcher = |url: &Url| -> Result<String, Error> {
    // Map URL to TerminusDB layer/graph
    let layer = terminusdb.get_layer_by_uri(url)?;

    // Return root node IDs as "pseudo-XML"
    // (xrust will wrap as nodes, not parse)
    Ok(format_root_nodes(layer))
};
```

**Pros**:
- ✅ No XML parsing after initial load
- ✅ Uses TerminusDB's multi-index infrastructure
- ✅ Native triple-store queries
- ✅ Single source of truth

**Cons**:
- ⚠️ Complex implementation (27 trait methods)
- ⚠️ Still limited by trait abstraction (no query optimization across trait boundary)
- 📊 **Performance**: ~5-20x faster than naive, still 5-10x slower than native DB

### Option 3: Native XQuery Engine in TerminusDB (Long-term)

**Idea**: Build XQuery compiler that generates native TerminusDB queries:

```xquery
collection("books")//book[@genre='fiction']/title
```

**Compiles to TerminusDB query plan**:
```rust
// Pseudo-code
layer.triples_p(ELEMENT_NAME)                    // All elements
    .filter(|t| id_object(t.object) == "book")   // Named "book"
    .filter(|t| {
        // Has attribute genre='fiction'
        layer.triples_sp(t.subject, HAS_ATTRIBUTE)
            .any(|attr|
                check_attribute(layer, attr.object, "genre", "fiction")
            )
    })
    .flat_map(|book| {
        // Get title children
        layer.triples_sp(book.subject, HAS_CHILD)
            .filter(|t| get_element_name(layer, t.object) == "title")
    })
```

**Optimization passes**:
1. **Predicate pushdown**: Filter early using indexes
2. **Index selection**: Choose optimal index (S, P, O, SP, etc.)
3. **Join reordering**: Minimize intermediate results
4. **Lazy evaluation**: Don't materialize until needed

**Pros**:
- ✅ Maximum performance - native triple-store queries
- ✅ Full control over optimization
- ✅ No abstraction overhead
- ✅ Can leverage TerminusDB's versioning, transactions
- 📊 **Performance**: Comparable to native XML DBs (within 2-5x)

**Cons**:
- ⚠️ Massive implementation effort (years)
- ⚠️ Must implement XQuery parser, compiler, optimizer
- ⚠️ Maintain compatibility with XQuery spec

## Realistic Recommendation for TerminusDB

### Phase 1: Proof of Concept (Option 2 - Deep Integration)

**Goal**: Get XPath/XQuery working, validate approach

```
Timeline: 3-4 months
Implementation:
- Fork xot, abstract indextree
- OR implement xrust Node trait on TerminusDB
- Add smart caching in closures
- Basic index support (element names, attributes)

Performance target: 10-50x faster than naive
Use cases: Small-medium datasets (<10K documents)
```

### Phase 2: Production Optimization (Option 2+)

**Goal**: Make it production-ready

```
Timeline: 6-12 months
Implementation:
- Add comprehensive indexes (paths, values, full-text)
- Query plan optimization
- Lazy evaluation and streaming
- Batch operations
- Memory optimization

Performance target: 5-10x slower than native XML DB
Use cases: Medium datasets (<100K documents), moderate query complexity
```

### Phase 3: Native Engine (Option 3) - IF Needed

**Goal**: Match production XML database performance

```
Timeline: 1-2 years
Implementation:
- XQuery → TerminusDB query compiler
- Cost-based query optimizer
- Advanced indexing strategies
- Parallel query execution

Performance target: Within 2-5x of BaseX/eXist-db
Use cases: Large datasets (millions of documents), complex queries
```

## The Pragmatic Path Forward

### Start with **Option 2 (Deep Integration)**:

**Why**:
1. ✅ **Realistic timeline** (3-6 months to working prototype)
2. ✅ **Reuses mature XPath engine** (xrust or xee)
3. ✅ **Acceptable performance** for many use cases
4. ✅ **Foundation for future optimization**
5. ✅ **No XML parsing overhead** after initial load

**Accept the trade-off**:
- Won't match BaseX/Oracle for large-scale queries initially
- Good enough for:
  - Document-centric queries (single document XPath)
  - Small-medium collections (<10K docs)
  - Moderate query complexity
  - Development/prototyping

**Optimize later**:
- Add indexes incrementally based on profiling
- Implement query rewrites for common patterns
- Consider native compiler if performance becomes critical

### Avoid **Option 3 (Native Engine)** initially:

**Unless**:
- You have 2-3 years and dedicated team
- XQuery is core to TerminusDB's value proposition
- Performance must match production XML DBs

## Key Insight

**Neither xrust nor xee solve the collection performance problem out of the box.**

Both delegate to application via closures:
- **xrust**: `fetcher` and `parser` closures
- **xee**: Would need similar mechanism for XQuery

**The real work is**:
1. Building the document cache/index in TerminusDB
2. Implementing smart closures that use it
3. Adding query optimization over time

**This is the SAME work regardless of which engine you choose.**

The difference is:
- **xrust**: More mature, trait-based abstraction easier to optimize
- **xee**: Better XPath 3.1 coverage, but needs XQuery implementation

## Bottom Line

Your concern is **100% valid** - discrete document parsing is a performance killer for XQuery.

**But**: This is a **solvable problem** that's independent of xrust vs xee choice:

1. **Short term**: Implement smart caching closures (weeks)
2. **Medium term**: Deep TerminusDB integration with indexes (months)
3. **Long term**: Native query compiler (years, if needed)

**Start with deep integration (Option 2)**, measure performance, optimize based on real workloads. You'll likely find it's "fast enough" for most use cases, and you can optimize hotspots incrementally.

---

## References

- [BaseX Databases Documentation](https://docs.basex.org/main/Databases)
- [Oracle XML DB XQuery](https://docs.oracle.com/en/database/oracle/oracle-database/21/adxdb/xquery-and-XML-DB.html)
- [XQuery Performance O'Reilly](https://www.oreilly.com/library/view/xquery/0596006349/ch15s06.html)
- [fn:collection() Reference](https://www.altova.com/xpath-xquery-reference/fn-collection)
- [xrust StaticContext](https://github.com/ballsteve/xrust/blob/main/src/transform/context.rs#L565)

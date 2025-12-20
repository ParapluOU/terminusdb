# Xee-Xot Integration Analysis: Document Loading & Node Construction

## Executive Summary

**Finding**: Xee uses xot primarily through **discrete document parsing**, but xot also supports **manual node construction**. This dual approach offers flexibility for TerminusDB integration.

## Integration Architecture

### Primary Path: Parse Discrete Documents

**Flow**:
```
User → Documents.add_string(uri, "<xml>...")
     → xot.parse(xml)                          [xot's XML parser]
     → Arena populated with nodes              [indextree]
     → Returns root Node
     → Stored as Document { uri, root }
```

**Source Evidence**:
```rust
// xee-xpath/src/documents.rs:34-42
pub fn add_string(
    &mut self,
    uri: &IriStr,
    xml: &str,
) -> Result<DocumentHandle, DocumentsError> {
    self.documents
        .borrow_mut()
        .add_string(&mut self.xot, Some(uri), xml)  // ← Delegates to internal Documents
}

// xee-interpreter/src/xml/document.rs:110-118
pub fn add_string(
    &mut self,
    xot: &mut Xot,
    uri: Option<&IriStr>,
    xml: &str,
) -> Result<DocumentHandle, DocumentsError> {
    let root = xot.parse(xml)?;  // ← KEY: Uses xot's parse() method
    self.add_root(uri, root)
}
```

### What `xot.parse()` Does

**Internal Implementation** (from `/tmp/xot/src/parse.rs`):

```rust
use xmlparser::{ElementEnd, StrSpan, Token, Tokenizer};

impl Xot {
    pub fn parse(&mut self, xml: &str) -> Result<Node, ParseError> {
        // Uses xmlparser crate for tokenization
        let tokenizer = Tokenizer::from(xml);

        // Builds DocumentBuilder which creates nodes in arena:
        let document = xot.arena.new_node(Value::Document);

        // Parses tokens and constructs tree:
        for token in tokenizer {
            match token {
                Token::ElementStart { prefix, local, .. } => {
                    // Creates element nodes
                }
                Token::Text { text } => {
                    // Creates text nodes
                }
                // ... etc
            }
        }

        Node::new(document)
    }
}
```

**Characteristics**:
- ✅ **In-memory string parsing** - entire XML string provided at once
- ✅ **Token-based** - uses `xmlparser` crate for tokenization
- ✅ **Arena population** - directly creates nodes via `arena.new_node(value)`
- ✅ **Returns root** - gives back document root Node handle
- ❌ **Not streaming** - no support for SAX-style streaming from disk
- ❌ **Not incremental** - can't feed partial documents

### Secondary Path: Manual Node Construction

**Xot provides manual construction APIs**:

```rust
// From xot/src/creation.rs:
impl Xot {
    pub fn new_document() -> Node { ... }

    pub fn new_element(&mut self, name: NameId) -> Node {
        let element = Value::Element(Element::new(name));
        self.new_node(element)
    }

    pub fn new_text(&mut self, text: &str) -> Node {
        let text = Value::Text(Text::new(text.to_string()));
        self.new_node(text)
    }

    pub fn new_comment(&mut self, text: &str) -> Node { ... }
    pub fn new_processing_instruction(...) -> Node { ... }

    // Manipulation
    pub fn append(&mut self, parent: Node, child: Node) -> Result<(), Error> {
        // Uses indextree's checked_append
    }

    pub fn prepend(&mut self, parent: Node, child: Node) -> Result<(), Error> { ... }
    pub fn insert_before(&mut self, ...) -> Result<(), Error> { ... }
    // ... etc
}
```

**Usage in xee** (29 occurrences):
- Primarily in **XSLT output construction**
- Creating result documents from transformations
- Building nodes programmatically rather than parsing

**Example**:
```rust
let mut xot = Xot::new();
let doc_name = xot.add_name("doc");
let doc_el = xot.new_element(doc_name);
let txt = xot.new_text("Hello, world!");
xot.append(doc_el, txt)?;

let document = xot.new_document();
xot.append(document, doc_el)?;

assert_eq!(xot.to_string(document)?, "<doc>Hello, world!</doc>");
```

## Document Model

### Documents Collection Structure

```rust
// High-level wrapper (xee-xpath)
pub struct Documents {
    pub(crate) xot: Xot,                    // Single shared arena
    pub(crate) documents: DocumentsRef,      // Collection of documents
}

// Internal structure (xee-interpreter)
pub struct Documents {
    documents: Vec<Document>,                // All documents in collection
    by_uri: HashMap<IriString, DocumentHandle>,  // URI lookup
    uri_by_document_node: HashMap<xot::Node, IriString>,
}

pub struct Document {
    uri: Option<IriString>,
    root: xot::Node,                         // Root node handle
}

pub struct DocumentHandle {
    documents_id: usize,                     // Which Documents collection
    id: usize,                               // Index in Vec<Document>
}
```

**Key insights**:
1. **Single Xot arena** shared across all documents in a collection
2. **Multiple document roots** in same arena
3. **URI-based lookup** for document retrieval
4. **Handle-based access** - opaque identifiers

### Document Lifecycle

```
1. Create collection:
   let mut documents = Documents::new();
   ↓ Creates Xot::new() (empty arena)

2. Add document:
   documents.add_string(uri, "<xml>...</xml>")
   ↓ xot.parse(xml)
   ↓ Populates arena with nodes
   ↓ Returns root Node
   ↓ Wraps in Document { uri, root }
   ↓ Returns DocumentHandle

3. Query document:
   query.execute(&mut documents, handle)
   ↓ Uses handle to get root Node
   ↓ Traverses arena via xot APIs
   ↓ Returns XPath results

4. Cleanup (optional):
   documents.cleanup(&mut documents.xot)
   ↓ Calls xot.remove(root) for each document
   ↓ Removes nodes from arena
```

## Integration Implications for TerminusDB

### Option 1: Parse-Only Integration

**Approach**: Implement xot abstraction, preserve `parse()` interface

```rust
trait TreeArena {
    fn parse(&mut self, xml: &str) -> Result<NodeHandle, Error>;
}

impl TreeArena for TerminusTreeArena {
    fn parse(&mut self, xml: &str) -> Result<u64, Error> {
        // Use xmlparser to tokenize
        let tokenizer = Tokenizer::from(xml);

        // Convert tokens to TerminusDB triples
        for token in tokenizer {
            match token {
                Token::ElementStart { local, .. } => {
                    let node_id = self.next_id();
                    self.add_triple(node_id, RDF_TYPE, XDOM_ELEMENT);
                    self.add_triple(node_id, XDOM_NAME, local);
                    // ... etc
                }
            }
        }

        Ok(root_node_id)
    }
}
```

**Pros**:
- ✅ Drop-in replacement for xot
- ✅ Minimal changes to xee
- ✅ Preserves existing API

**Cons**:
- ⚠️ Doesn't leverage existing TerminusDB data
- ⚠️ Still requires parsing XML strings
- ⚠️ Can't query TerminusDB data directly with XPath

### Option 2: Manual Construction Integration

**Approach**: Build xot tree from TerminusDB data programmatically

```rust
// Read data from TerminusDB
let layer = store.head()?;

// Construct xot tree manually
let mut xot = Xot::new();
let doc = xot.new_document();

for triple in layer.triples_p(HAS_CHILD_PREDICATE) {
    let element_name = get_name(layer, triple.subject)?;
    let name_id = xot.add_name(element_name);
    let element = xot.new_element(name_id);

    // Add children recursively
    build_subtree(&mut xot, element, triple.subject, layer)?;

    xot.append(doc, element)?;
}

// Now can run XPath queries on xot tree
let queries = Queries::default();
let q = queries.one("//book[@genre='fiction']", ...)?;
let result = q.execute(&mut documents, doc)?;
```

**Pros**:
- ✅ Can expose existing TerminusDB data to XPath
- ✅ No XML parsing needed if data already in TerminusDB
- ✅ Flexible - construct tree as needed

**Cons**:
- ⚠️ Copying data from TerminusDB to xot arena (duplication)
- ⚠️ Memory overhead (two representations)
- ⚠️ Requires bidirectional sync if tree is modified

### Option 3: Deep Abstraction (Recommended Path)

**Approach**: Abstract xot's arena to use TerminusDB storage directly

```rust
// Replace indextree::Arena with TerminusDB-backed arena
trait TreeArena {
    type NodeHandle;

    fn new_node(&mut self, value: Value) -> Self::NodeHandle;
    fn get(&self, handle: Self::NodeHandle) -> Option<&Value>;
    // ... etc
}

// TerminusDB implementation
impl TreeArena for TerminusTreeArena {
    type NodeHandle = u64;

    fn new_node(&mut self, value: Value) -> u64 {
        let node_id = self.next_id();
        self.encode_value_as_triples(node_id, value);
        node_id
    }

    fn get(&self, handle: u64) -> Option<&Value> {
        // Reconstruct Value from triples
        self.decode_value_from_triples(handle)
    }
}
```

**This allows BOTH paths**:

```rust
// Path A: Parse XML strings into TerminusDB
documents.add_string(uri, "<xml>...")?;
↓ Uses TerminusDB-backed parse() implementation

// Path B: Expose existing TerminusDB data
let root = documents.add_root(uri, existing_terminusdb_node)?;
↓ Wraps existing TerminusDB node as document root
```

**Pros**:
- ✅ No data duplication
- ✅ Direct XPath queries on TerminusDB data
- ✅ Can parse new documents into TerminusDB
- ✅ Single source of truth

**Cons**:
- ⚠️ Requires full xot abstraction (as analyzed in previous doc)
- ⚠️ Performance overhead from triple encoding/decoding
- ⚠️ More complex implementation

## Streaming & Incremental Loading

### Current Limitations

**Xee/Xot does NOT support**:
- ❌ **Streaming from disk** - no SAX-style event-driven parsing
- ❌ **Incremental loading** - can't append to existing document
- ❌ **Lazy loading** - entire document parsed into memory
- ❌ **Chunked parsing** - no partial document support

**Architecture reasons**:
1. `xot.parse()` takes `&str` - entire string must be in memory
2. `xmlparser::Tokenizer` operates on complete strings
3. Arena model assumes all nodes present
4. No streaming iterator pattern

### What IS Supported

**Multiple discrete documents**:
```rust
// Can add many documents to same collection
documents.add_string(uri1, xml1)?;
documents.add_string(uri2, xml2)?;
documents.add_string(uri3, xml3)?;

// All share same arena, separate roots
// Can query across documents with fn:doc()
```

**Fragment parsing**:
```rust
// Can parse XML fragments (no <?xml?> declaration)
documents.add_fragment_string(xml_fragment)?;
```

**Manual incremental construction**:
```rust
// Can build tree incrementally via API
let doc = xot.new_document();
let root = xot.new_element(name);

// Add children over time
for item in items {
    let child = xot.new_element(item.name);
    xot.append(root, child)?;
}

xot.append(doc, root)?;
```

## XPath Execution Model

### Query Compilation & Execution

```rust
// Step 1: Compile XPath expression (once)
let queries = Queries::default();
let q = queries.one("/root/child", |_, item| {
    Ok(item.try_into_value::<String>()?)
})?;

// Step 2: Execute against document (many times)
let result = q.execute(&mut documents, doc_handle)?;
```

**Internal flow**:
```
1. XPath parsing → AST
2. AST → Intermediate Representation (IR)
3. IR → Bytecode
4. Bytecode interpreter executes against xot arena:
   - Uses xot navigation APIs (children(), parent(), etc.)
   - Accesses node values via xot.value(node)
   - Performs XPath axis traversal
   - Evaluates predicates
   - Returns sequence of Items
```

**Key observation**: XPath engine uses **xot's public API**, not direct arena access. This is good for abstraction!

### Node Access Patterns

**XPath operations map to xot methods**:

```rust
// Axis traversal
child::element → xot.children(node).filter(xot.is_element)
parent::node() → xot.parent(node)
descendant::* → xot.descendants(node)
attribute::* → xot.attribute_nodes(node)

// Node tests
name() → xot.name(node)
text() → xot.text_content_str(node)
node-type() → xot.value(node).value_type()

// Predicates
[@attr='val'] → xot.attributes(node).get(attr_name)
[position()=1] → iteration with index
```

**This means**: Abstracting xot's navigation APIs covers XPath execution needs.

## Data Flow Summary

### Current (Xee + Xot + Indextree)

```
XML String
    ↓ [xmlparser tokenization]
xot.parse()
    ↓ [arena.new_node() calls]
indextree::Arena<Value>
    ↓ [NodeId handles]
xot::Node wrappers
    ↓ [xot navigation APIs]
XPath interpreter
    ↓ [bytecode execution]
Sequence<Item> results
```

### Proposed (Xee + Xot Abstraction + TerminusDB)

```
Option A: Parse XML
XML String
    ↓ [xmlparser tokenization]
xot_abstraction.parse()
    ↓ [triple encoding]
TerminusDB Layer
    ↓ [u64 node IDs]
xot::Node wrappers
    ↓ [trait methods → triple queries]
XPath interpreter
    ↓ [bytecode execution]
Sequence<Item> results

Option B: Existing Data
TerminusDB Layer
    ↓ [expose as root Node]
documents.add_root()
    ↓ [u64 node IDs]
xot::Node wrappers
    ↓ [trait methods → triple queries]
XPath interpreter
    ↓ [bytecode execution]
Sequence<Item> results
```

## Conclusion

### How Xee Uses Xot

1. **Primary**: Parse discrete XML documents via `xot.parse(xml_string)`
2. **Secondary**: Manual construction via `new_element()`, `append()`, etc.
3. **Model**: Multiple document roots in single shared arena
4. **Access**: URI-based document handles, node-based traversal
5. **Execution**: XPath queries use xot's public navigation API

### Integration Strategy Implications

**For TerminusDB integration, the abstraction layer must support**:

✅ **Required**:
- `parse(xml: &str)` - to load XML documents
- Node creation APIs - for XSLT output construction
- Navigation methods - for XPath axis traversal
- Value access - to read element names, text content, etc.

⚠️ **Optional** (but valuable):
- `add_root(existing_node)` - to expose TerminusDB data directly
- Bidirectional mapping - to sync changes back to TerminusDB
- Lazy loading - to avoid full tree materialization

🔧 **Architecture Decision**:

**Start with Option 3 (Deep Abstraction)** but implement in phases:

1. **Phase 1**: Basic abstraction - parse XML into TerminusDB
2. **Phase 2**: Manual construction - XSLT output support
3. **Phase 3**: Expose existing data - `add_root()` for TerminusDB nodes
4. **Phase 4**: Optimization - caching, lazy loading, etc.

This incremental approach gets XPath working quickly while preserving path to full integration.

---

## Code References

**Xee Integration**:
- `/tmp/xee/xee-xpath/src/documents.rs:34-42` - Documents.add_string()
- `/tmp/xee/xee-interpreter/src/xml/document.rs:110-118` - Internal add_string()
- `/tmp/xee/xee-interpreter/src/xml/document.rs:42-56` - Document structure

**Xot Parsing**:
- `/tmp/xot/src/parse.rs:1-100` - Parser implementation with xmlparser
- `/tmp/xot/src/xotdata.rs:50-103` - Xot struct and Arena integration

**Xot Manual Construction**:
- `/tmp/xot/src/creation.rs:14-200` - Node creation APIs
- `/tmp/xot/src/manipulation.rs:62-300` - Tree manipulation APIs

**XPath Execution**:
- `/tmp/xee/xee-interpreter/src/xml/step.rs` - Axis traversal
- `/tmp/xee/xee-interpreter/src/interpreter/interpret.rs` - Bytecode execution

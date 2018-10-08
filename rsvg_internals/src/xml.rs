use encoding::label::encoding_from_whatwg_label;
use encoding::DecoderTrap;
use libc;
use std;
use std::cell::RefCell;
use std::ptr;
use std::rc::Rc;
use std::str;

use attributes::Attribute;
use css;
use handle::{self, RsvgHandle};
use load::rsvg_load_new_node;
use node::{node_new, Node, NodeType};
use property_bag::PropertyBag;
use structure::NodeSvg;
use text::NodeChars;
use tree::{RsvgTree, Tree};
use util::utf8_cstr;

/// A trait for processing a certain kind of XML subtree
///
/// In the "normal" state of processing, an `XmlHandler` may create an RsvgNode
/// for each SVG element it finds, and create NodeChars inside those nodes when it
/// encounters character data.
///
/// There may be other, special contexts for different subtrees, for example,
/// for the `<style>` element.
trait XmlHandler {
    /// Called when the XML parser sees the beginning of an element
    fn start_element(
        &self,
        previous_handler: Option<&XmlHandler>,
        parent: Option<&Rc<Node>>,
        handle: *mut RsvgHandle,
        name: &str,
        pbag: &PropertyBag,
    ) -> Box<XmlHandler>;

    /// Called when the XML parser sees the end of an element.
    fn end_element(&self, handle: *mut RsvgHandle, name: &str) -> Option<Rc<Node>>;

    /// Called when the XML parser sees character data or CDATA
    fn characters(&self, text: &str);

    fn get_node(&self) -> Option<Rc<Node>> {
        None
    }
}

struct NodeCreationContext {
    node: Option<Rc<Node>>,
}

impl XmlHandler for NodeCreationContext {
    fn start_element(
        &self,
        _previous_handler: Option<&XmlHandler>,
        parent: Option<&Rc<Node>>,
        handle: *mut RsvgHandle,
        name: &str,
        pbag: &PropertyBag,
    ) -> Box<XmlHandler> {
        match name {
            "include" => {
                let ctx = XIncludeContext::empty();
                ctx.start_element(Some(self), parent, handle, name, pbag)
            }

            "style" => {
                let ctx = StyleContext::empty();
                ctx.start_element(Some(self), parent, handle, name, pbag)
            }

            _ => {
                let node = self.create_node(parent, handle, name, pbag);

                Box::new(NodeCreationContext { node: Some(node) })
            }
        }
    }

    fn end_element(&self, handle: *mut RsvgHandle, _name: &str) -> Option<Rc<Node>> {
        let node = self.node.as_ref().unwrap().clone();

        // The "svg" node is special; it parses its style attributes
        // here, not during element creation.
        if node.get_type() == NodeType::Svg {
            node.with_impl(|svg: &NodeSvg| {
                svg.set_delayed_style(&node, handle);
            });
        }

        Some(node)
    }

    fn characters(&self, text: &str) {
        let node = self.node.as_ref().unwrap();

        if text.len() == 0 {
            return;
        }

        if node.accept_chars() {
            let chars_node = if let Some(child) = node.find_last_chars_child() {
                child
            } else {
                let child = node_new(
                    NodeType::Chars,
                    Some(&node),
                    "rsvg-chars",
                    None,
                    None,
                    Box::new(NodeChars::new()),
                );
                node.add_child(&child);
                child
            };

            chars_node.with_impl(|chars: &NodeChars| {
                chars.append(text);
            });
        }
    }

    fn get_node(&self) -> Option<Rc<Node>> {
        Some(self.node.as_ref().unwrap().clone())
    }
}

impl NodeCreationContext {
    fn empty() -> NodeCreationContext {
        NodeCreationContext { node: None }
    }

    fn create_node(
        &self,
        parent: Option<&Rc<Node>>,
        handle: *mut RsvgHandle,
        name: &str,
        pbag: &PropertyBag,
    ) -> Rc<Node> {
        let mut defs = handle::get_defs(handle);

        let new_node = rsvg_load_new_node(name, parent, pbag, &mut defs);

        if let Some(parent) = parent {
            parent.add_child(&new_node);
        }

        new_node.set_atts(&new_node, handle, pbag);

        // The "svg" node is special; it will parse its style attributes
        // until the end, in standard_element_end().
        if new_node.get_type() != NodeType::Svg {
            new_node.set_style(handle, pbag);
        }

        new_node.set_overridden_properties();

        new_node
    }
}

/// Handles the `<style>` element by parsing its character contents as CSS
struct StyleContext {
    is_text_css: bool,
    text: RefCell<String>,
}

impl XmlHandler for StyleContext {
    fn start_element(
        &self,
        _previous_handler: Option<&XmlHandler>,
        _parent: Option<&Rc<Node>>,
        _handle: *mut RsvgHandle,
        _name: &str,
        pbag: &PropertyBag,
    ) -> Box<XmlHandler> {
        // FIXME: See these:
        //
        // https://www.w3.org/TR/SVG/styling.html#StyleElementTypeAttribute
        // https://www.w3.org/TR/SVG/styling.html#ContentStyleTypeAttribute
        //
        // If the "type" attribute is not present, we should fallback to the
        // "contentStyleType" attribute of the svg element, which in turn
        // defaults to "text/css".
        //
        // See where is_text_css is used to see where we parse the contents
        // of the style element.

        let mut is_text_css = true;

        for (_key, attr, value) in pbag.iter() {
            if attr == Attribute::Type {
                is_text_css = value == "text/css";
            }
        }

        Box::new(StyleContext {
            is_text_css,
            text: RefCell::new(String::new()),
        })
    }

    fn end_element(&self, handle: *mut RsvgHandle, _name: &str) -> Option<Rc<Node>> {
        if self.is_text_css {
            let text = self.text.borrow();
            css::parse_into_handle(handle, &text);
        }

        None
    }

    fn characters(&self, text: &str) {
        self.text.borrow_mut().push_str(text);
    }
}

impl StyleContext {
    fn empty() -> StyleContext {
        StyleContext {
            is_text_css: false,
            text: RefCell::new(String::new()),
        }
    }
}

struct XIncludeContext {
    needs_fallback: bool,
}

impl XmlHandler for XIncludeContext {
    fn start_element(
        &self,
        _previous_handler: Option<&XmlHandler>,
        _parent: Option<&Rc<Node>>,
        handle: *mut RsvgHandle,
        _name: &str,
        pbag: &PropertyBag,
    ) -> Box<XmlHandler> {
        let mut href = None;
        let mut parse = None;
        let mut encoding = None;

        for (_key, attr, value) in pbag.iter() {
            match attr {
                Attribute::Href => href = Some(value),
                Attribute::Parse => parse = Some(value),
                Attribute::Encoding => encoding = Some(value),
                _ => (),
            }
        }

        self.acquire(handle, href, parse, encoding);

        unimplemented!("finish start_xinclude() here");

        Box::new(XIncludeContext::empty())
    }

    fn end_element(&self, handle: *mut RsvgHandle, _name: &str) -> Option<Rc<Node>> {
        unimplemented!();
    }

    fn characters(&self, text: &str) {
        unimplemented!();
    }
}

impl XIncludeContext {
    fn empty() -> XIncludeContext {
        XIncludeContext {
            needs_fallback: true,
        }
    }

    fn acquire(
        &self,
        handle: *mut RsvgHandle,
        href: Option<&str>,
        parse: Option<&str>,
        encoding: Option<&str>,
    ) {
        if let Some(href) = href {
            if parse == Some("text") {
                self.acquire_text(handle, href, encoding);
            } else {
                unimplemented!("finish the xml case here");
            }
        }
    }

    fn acquire_text(&self, handle: *mut RsvgHandle, href: &str, encoding: Option<&str>) {
        let binary = match handle::acquire_data(handle, href) {
            Ok(b) => b,
            Err(e) => {
                rsvg_log!("could not acquire \"{}\": {}", href, e);
                return;
            }
        };

        let encoding = encoding.unwrap_or("utf-8");

        let encoder = match encoding_from_whatwg_label(encoding) {
            Some(enc) => enc,
            None => {
                rsvg_log!("unknown encoding \"{}\" for \"{}\"", encoding, href);
                return;
            }
        };

        let utf8_data = match encoder.decode(&binary.data, DecoderTrap::Strict) {
            Ok(data) => data,

            Err(e) => {
                rsvg_log!(
                    "could not convert contents of \"{}\" from character encoding \"{}\": {}",
                    href,
                    encoding,
                    e
                );
                return;
            }
        };

        unimplemented!("rsvg_xml_state_characters(utf8_data)");
    }
}

/// A concrete parsing context for a surrounding `element_name` and its XML event handlers
struct Context {
    element_name: String,
    handler: Box<XmlHandler>,
}

// A *const RsvgXmlState is just the type that we export to C
pub enum RsvgXmlState {}

/// Holds the state used for XML processing
///
/// These methods are called when an XML event is parsed out of the XML stream: `start_element`,
/// `end_element`, `characters`.
///
/// When an element starts, we push a corresponding `Context` into the `context_stack`.  Within
/// that context, all XML events will be forwarded to it, and processed in one of the `XmlHandler`
/// trait objects. Normally the context refers to a `NodeCreationContext` implementation which is
/// what creates normal graphical elements.
///
/// When we get to a `<style>` element, we push a `StyleContext`, which processes its contents
/// specially.
struct XmlState {
    tree: Option<Box<Tree>>,

    context_stack: Vec<Context>,
}

impl XmlState {
    fn new() -> XmlState {
        XmlState {
            tree: None,
            context_stack: Vec::new(),
        }
    }

    pub fn set_root(&mut self, root: &Rc<Node>) {
        if self.tree.is_some() {
            panic!("The tree root has already been set");
        }

        self.tree = Some(Box::new(Tree::new(root)));
    }

    pub fn steal_tree(&mut self) -> Option<Box<Tree>> {
        self.tree.take()
    }

    pub fn start_element(&mut self, handle: *mut RsvgHandle, name: &str, pbag: &PropertyBag) {
        let next_context = if let Some(top) = self.context_stack.last() {
            top.handler.start_element(
                Some(&*top.handler),
                top.handler.get_node().as_ref(),
                handle,
                name,
                pbag,
            )
        } else {
            let default_context = NodeCreationContext::empty();

            default_context.start_element(None, None, handle, name, pbag)
        };

        let context = Context {
            element_name: name.to_string(),
            handler: next_context,
        };

        self.context_stack.push(context);
    }

    pub fn end_element(&mut self, handle: *mut RsvgHandle, name: &str) {
        if let Some(top) = self.context_stack.pop() {
            assert!(name == top.element_name);

            if let Some(node) = top.handler.end_element(handle, name) {
                if self.context_stack.is_empty() {
                    self.set_root(&node);
                }
            }
        } else {
            panic!("end_element: XML handler stack is empty!?");
        }
    }

    pub fn characters(&mut self, text: &str) {
        if let Some(top) = self.context_stack.last() {
            top.handler.characters(text);
        } else {
            panic!("characters: XML handler stack is empty!?");
        }
    }
}

#[no_mangle]
pub extern "C" fn rsvg_xml_state_new() -> *mut RsvgXmlState {
    Box::into_raw(Box::new(XmlState::new())) as *mut RsvgXmlState
}

#[no_mangle]
pub extern "C" fn rsvg_xml_state_free(xml: *mut RsvgXmlState) {
    assert!(!xml.is_null());
    let xml = unsafe { &mut *(xml as *mut XmlState) };
    unsafe {
        Box::from_raw(xml);
    }
}

#[no_mangle]
pub extern "C" fn rsvg_xml_state_steal_tree(xml: *mut RsvgXmlState) -> *mut RsvgTree {
    assert!(!xml.is_null());
    let xml = unsafe { &mut *(xml as *mut XmlState) };

    if let Some(tree) = xml.steal_tree() {
        Box::into_raw(tree) as *mut RsvgTree
    } else {
        ptr::null_mut()
    }
}

#[no_mangle]
pub extern "C" fn rsvg_xml_state_start_element(
    xml: *mut RsvgXmlState,
    handle: *mut RsvgHandle,
    name: *const libc::c_char,
    pbag: *const PropertyBag,
) {
    assert!(!xml.is_null());
    let xml = unsafe { &mut *(xml as *mut XmlState) };

    assert!(!name.is_null());
    let name = unsafe { utf8_cstr(name) };

    assert!(!pbag.is_null());
    let pbag = unsafe { &*pbag };

    xml.start_element(handle, name, pbag);
}

#[no_mangle]
pub extern "C" fn rsvg_xml_state_end_element(
    xml: *mut RsvgXmlState,
    handle: *mut RsvgHandle,
    name: *const libc::c_char,
) {
    assert!(!xml.is_null());
    let xml = unsafe { &mut *(xml as *mut XmlState) };

    assert!(!name.is_null());
    let name = unsafe { utf8_cstr(name) };

    xml.end_element(handle, name);
}

#[no_mangle]
pub extern "C" fn rsvg_xml_state_characters(
    xml: *mut RsvgXmlState,
    unterminated_text: *const libc::c_char,
    len: usize,
) {
    assert!(!xml.is_null());
    let xml = unsafe { &mut *(xml as *mut XmlState) };

    assert!(!unterminated_text.is_null());

    // libxml2 already validated the incoming string as UTF-8.  Note that
    // it is *not* nul-terminated; this is why we create a byte slice first.
    let bytes = unsafe { std::slice::from_raw_parts(unterminated_text as *const u8, len) };
    let utf8 = unsafe { str::from_utf8_unchecked(bytes) };

    xml.characters(utf8);
}

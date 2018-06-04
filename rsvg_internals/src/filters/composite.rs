use std::cell::{Cell, RefCell};

use cairo::{self, ImageSurface};
use libc::c_char;

use attributes::Attribute;
use error::{AttributeError, NodeError};
use handle::RsvgHandle;
use node::{boxed_node_new, NodeResult, NodeTrait, NodeType, RsvgCNodeImpl, RsvgNode};
use parsers::{self, parse, Parse};
use property_bag::PropertyBag;
use srgb::{linearize_surface, unlinearize_surface};
use util::clamp;

use super::context::{FilterContext, FilterOutput, FilterResult};
use super::input::Input;
use super::iterators::{ImageSurfaceDataShared, Pixels};
use super::{get_surface, Filter, FilterError, PrimitiveWithInput};

/// Enumeration of the possible compositing operations.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
enum Operator {
    Over,
    In,
    Out,
    Atop,
    Xor,
    Arithmetic,
}

/// The `feComposite` filter primitive.
struct Composite {
    base: PrimitiveWithInput,
    in2: RefCell<Option<Input>>,
    operator: Cell<Operator>,
    k1: Cell<f64>,
    k2: Cell<f64>,
    k3: Cell<f64>,
    k4: Cell<f64>,
}

impl Composite {
    /// Constructs a new `Composite` with empty properties.
    #[inline]
    fn new() -> Composite {
        Composite {
            base: PrimitiveWithInput::new::<Self>(),
            in2: RefCell::new(None),
            operator: Cell::new(Operator::Over),
            k1: Cell::new(0f64),
            k2: Cell::new(0f64),
            k3: Cell::new(0f64),
            k4: Cell::new(0f64),
        }
    }
}

impl NodeTrait for Composite {
    fn set_atts(
        &self,
        node: &RsvgNode,
        handle: *const RsvgHandle,
        pbag: &PropertyBag,
    ) -> NodeResult {
        self.base.set_atts(node, handle, pbag)?;

        for (_key, attr, value) in pbag.iter() {
            match attr {
                Attribute::In2 => {
                    self.in2.replace(Some(parse("in2", value, (), None)?));
                }
                Attribute::Operator => self.operator.set(parse("operator", value, (), None)?),
                Attribute::K1 => self
                    .k1
                    .set(parsers::number(value).map_err(|err| NodeError::parse_error(attr, err))?),
                Attribute::K2 => self
                    .k2
                    .set(parsers::number(value).map_err(|err| NodeError::parse_error(attr, err))?),
                Attribute::K3 => self
                    .k3
                    .set(parsers::number(value).map_err(|err| NodeError::parse_error(attr, err))?),
                Attribute::K4 => self
                    .k4
                    .set(parsers::number(value).map_err(|err| NodeError::parse_error(attr, err))?),
                _ => (),
            }
        }

        Ok(())
    }

    #[inline]
    fn get_c_impl(&self) -> *const RsvgCNodeImpl {
        self.base.get_c_impl()
    }
}

impl Filter for Composite {
    fn render(&self, _node: &RsvgNode, ctx: &FilterContext) -> Result<FilterResult, FilterError> {
        let bounds = self.base.get_bounds(ctx);

        let input_surface = get_surface(self.base.get_input(ctx))?;
        let input_2_surface = get_surface(ctx.get_input(self.in2.borrow().as_ref()))?;

        // It's important to linearize sRGB before doing any blending, since otherwise the colors
        // will be darker than they should be.
        let input_surface =
            linearize_surface(&input_surface, bounds).map_err(FilterError::BadInputSurfaceStatus)?;

        let output_surface = if self.operator.get() == Operator::Arithmetic {
            let input_data = ImageSurfaceDataShared::new(&input_surface)?;
            let input_2_data = ImageSurfaceDataShared::new(&input_2_surface)?;

            let mut output_surface = ImageSurface::create(
                cairo::Format::ARgb32,
                input_data.width as i32,
                input_data.height as i32,
            ).map_err(FilterError::OutputSurfaceCreation)?;

            let output_stride = output_surface.get_stride() as usize;
            {
                let mut output_data = output_surface.get_data().unwrap();

                let k1 = self.k1.get();
                let k2 = self.k2.get();
                let k3 = self.k3.get();
                let k4 = self.k4.get();

                for (x, y, pixel, pixel_2) in Pixels::new(input_data, bounds)
                    .map(|(x, y, p)| (x, y, p, input_2_data.get_pixel(x, y)))
                {
                    let i1a = f64::from(pixel.a) / 255f64;
                    let i2a = f64::from(pixel_2.a) / 255f64;
                    let oa = k1 * i1a * i2a + k2 * i1a + k3 * i2a + k4;
                    let oa = clamp(oa, 0f64, 1f64);

                    let output_base = y * output_stride + 4 * x;

                    // Contents of image surfaces are transparent by default, so if the
                    // resulting pixel is transparent there's no need
                    // to do anything.
                    if oa > 0f64 {
                        output_data[output_base + 3] = (oa * 255f64).round() as u8;

                        // TODO: make this much better with mutable pixel iterators for output.
                        for (ch, &(i1, i2)) in [
                            (pixel.r, pixel_2.r),
                            (pixel.g, pixel_2.g),
                            (pixel.b, pixel_2.b),
                        ].iter()
                            .enumerate()
                        {
                            let i1 = f64::from(i1) / 255f64;
                            let i2 = f64::from(i2) / 255f64;

                            let o = k1 * i1 * i2 + k2 * i1 + k3 * i2 + k4;
                            let o = clamp(o, 0f64, oa);

                            let o = (o * 255f64).round() as u8;
                            output_data[output_base + ch] = o;
                        }
                    }
                }
            }

            output_surface
        } else {
            let output_surface = linearize_surface(&input_2_surface, bounds)
                .map_err(FilterError::BadInputSurfaceStatus)?;

            let cr = cairo::Context::new(&output_surface);
            cr.rectangle(
                bounds.x0 as f64,
                bounds.y0 as f64,
                (bounds.x1 - bounds.x0) as f64,
                (bounds.y1 - bounds.y0) as f64,
            );
            cr.clip();

            cr.set_source_surface(&input_surface, 0f64, 0f64);
            cr.set_operator(self.operator.get().into());
            cr.paint();

            output_surface
        };

        let output_surface = unlinearize_surface(&output_surface, bounds)
            .map_err(FilterError::OutputSurfaceCreation)?;

        Ok(FilterResult {
            name: self.base.result.borrow().clone(),
            output: FilterOutput {
                surface: output_surface,
                bounds,
            },
        })
    }
}

impl Parse for Operator {
    type Data = ();
    type Err = AttributeError;

    fn parse(s: &str, _data: Self::Data) -> Result<Self, Self::Err> {
        match s {
            "over" => Ok(Operator::Over),
            "in" => Ok(Operator::In),
            "out" => Ok(Operator::Out),
            "atop" => Ok(Operator::Atop),
            "xor" => Ok(Operator::Xor),
            "arithmetic" => Ok(Operator::Arithmetic),
            _ => Err(AttributeError::Value("invalid operator value".to_string())),
        }
    }
}

impl From<Operator> for cairo::Operator {
    #[inline]
    fn from(x: Operator) -> Self {
        match x {
            Operator::Over => cairo::Operator::Over,
            Operator::In => cairo::Operator::In,
            Operator::Out => cairo::Operator::Out,
            Operator::Atop => cairo::Operator::Atop,
            Operator::Xor => cairo::Operator::Xor,
            _ => panic!("can't convert Operator::Arithmetic to a cairo::Operator"),
        }
    }
}

/// Returns a new `feComposite` node.
#[no_mangle]
pub unsafe extern "C" fn rsvg_new_filter_primitive_composite(
    _element_name: *const c_char,
    parent: *mut RsvgNode,
) -> *mut RsvgNode {
    let filter = Composite::new();
    boxed_node_new(NodeType::FilterPrimitiveComposite, parent, Box::new(filter))
}

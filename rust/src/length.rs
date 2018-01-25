use ::cssparser::{Parser, ParserInput, Token};
use ::glib::translate::*;
use ::libc;

use std::f64::consts::*;

use drawing_ctx;
use drawing_ctx::RsvgDrawingCtx;
use parsers::Parse;
use parsers::ParseError;
use error::*;
use util::utf8_cstr;

/* Keep this in sync with ../../rsvg-private.h:LengthUnit */
#[repr(C)]
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum LengthUnit {
    Default,
    Percent,
    FontEm,
    FontEx,
    Inch,
    RelativeLarger,
    RelativeSmaller
}

/* Keep this in sync with ../../rsvg-private.h:LengthDir */
#[repr(C)]
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum LengthDir {
    Horizontal,
    Vertical,
    Both
}

/* This is *not* an opaque struct; it is actually visible to the C code.  It is so
 * that the remaining C code can create RsvgLength values as part of existing
 * structures or objects, without allocations on the heap.
 */
/* Keep this in sync with ../../rsvg-private.h:RsvgLength */
#[repr(C)]
#[derive(Debug, PartialEq, Copy, Clone)]
pub struct RsvgLength {
    pub length: f64,
    pub unit: LengthUnit,
    dir: LengthDir
}

impl Default for RsvgLength {
    fn default () -> RsvgLength {
        RsvgLength {
            length: 0.0,
            unit:   LengthUnit::Default,
            dir:    LengthDir::Both
        }
    }
}

const POINTS_PER_INCH: f64 = 72.0;
const CM_PER_INCH:     f64 = 2.54;
const MM_PER_INCH:     f64 = 25.4;
const PICA_PER_INCH:   f64 = 6.0;

fn compute_named_size (name: &str) -> f64 {
    let power: f64;

    match name {
        "xx-small" => { power = -3.0; },
        "x-small"  => { power = -2.0; },
        "small"    => { power = -1.0; },
        "medium"   => { power = 0.0; },
        "large"    => { power = 1.0; },
        "x-large"  => { power = 2.0; },
        "xx-large" => { power = 3.0; },
        _          => { unreachable! (); }
    }

    12.0 * 1.2f64.powf (power) / POINTS_PER_INCH
}

#[no_mangle]
pub extern fn rsvg_length_parse (string: *const libc::c_char, dir: LengthDir) -> RsvgLength {
    let my_string = unsafe { utf8_cstr(string) };

    // FIXME: this ignores errors; propagate them upstream
    RsvgLength::parse (my_string, dir).unwrap_or_else(|_| {RsvgLength::default ()})
}

/* https://www.w3.org/TR/SVG/types.html#DataTypeLength
 * https://www.w3.org/TR/2008/REC-CSS2-20080411/syndata.html#length-units
 *
 * Lengths have units.  When they need to be need resolved to
 * units in the user's coordinate system, some unit types
 * need to know if they are horizontal/vertical/both.  For example,
 * a some_object.width="50%" is 50% with respect to the current
 * viewport's width.  In this case, the @dir argument is used
 * inside RsvgLength::normalize(), when it needs to know to what the
 * length refers.
 */

fn make_err () -> AttributeError {
    AttributeError::Parse (ParseError::new ("expected length: number(\"em\" | \"ex\" | \"px\" | \"in\" | \"cm\" | \"mm\" | \"pt\" | \"pc\" | \"%\")?"))
}

impl Parse for RsvgLength {
    type Data = LengthDir;
    type Err = AttributeError;

    fn parse (string: &str, dir: LengthDir) -> Result <RsvgLength, AttributeError> {
        let mut input = ParserInput::new (string);
        let mut parser = Parser::new (&mut input);

        let length = RsvgLength::from_cssparser(&mut parser, dir)?;

        parser.expect_exhausted ().map_err (|_| make_err ())?;

        Ok(length)
    }
}

impl RsvgLength {
    pub fn new(l: f64, unit: LengthUnit, dir: LengthDir) -> RsvgLength {
        RsvgLength {
            length: l,
            unit: unit,
            dir: dir
        }
    }

    pub fn check_nonnegative (self) -> Result <RsvgLength, AttributeError> {
        if self.length >= 0.0 {
            Ok (self)
        } else {
            Err (AttributeError::Value ("value must be non-negative".to_string ()))
        }
    }

    pub fn normalize (&self, draw_ctx: *const RsvgDrawingCtx) -> f64 {
        match self.unit {
            LengthUnit::Default => {
                self.length
            },

            LengthUnit::Percent => {
                let (width, height) = drawing_ctx::get_view_box_size (draw_ctx);

                match self.dir {
                    LengthDir::Horizontal => { self.length * width },
                    LengthDir::Vertical   => { self.length * height },
                    LengthDir::Both       => { self.length * viewport_percentage (width, height) }
                }
            },

            LengthUnit::FontEm => {
                self.length * drawing_ctx::get_normalized_font_size (draw_ctx)
            },

            LengthUnit::FontEx => {
                self.length * drawing_ctx::get_normalized_font_size (draw_ctx) / 2.0
            },

            LengthUnit::Inch => {
                let (dpi_x, dpi_y) = drawing_ctx::get_dpi (draw_ctx);

                match self.dir {
                    LengthDir::Horizontal => { self.length * dpi_x },
                    LengthDir::Vertical   => { self.length * dpi_y },
                    LengthDir::Both       => { self.length * viewport_percentage (dpi_x, dpi_y) }
                }
            },

            // FIXME: these are pending: https://www.w3.org/TR/2008/REC-CSS2-20080411/fonts.html#propdef-font-size
            LengthUnit::RelativeLarger |
            LengthUnit::RelativeSmaller => { 0.0 }
        }
    }

    pub fn hand_normalize (&self,
                           pixels_per_inch: f64,
                           width_or_height: f64,
                           font_size: f64) -> f64 {
        match self.unit {
            LengthUnit::Default => { self.length },

            LengthUnit::Percent => { self.length * width_or_height },

            LengthUnit::FontEm => { self.length * font_size },

            LengthUnit::FontEx => { self.length * font_size / 2.0 },

            LengthUnit::Inch => { self.length * pixels_per_inch },

            _ => { 0.0 }
        }
    }

    fn from_cssparser(parser: &mut Parser, dir: LengthDir) -> Result <RsvgLength, AttributeError> {

        let length = {
            let token = parser.next ()
                .map_err (|_| AttributeError::Parse (ParseError::new ("expected number and optional symbol, or number and percentage")))?;

            match *token {
                Token::Number { value, .. } => RsvgLength { length: f64::from(value),
                                                            unit:   LengthUnit::Default,
                                                            dir:    dir },

                Token::Percentage { unit_value, .. } => RsvgLength { length: f64::from(unit_value),
                                                                     unit:   LengthUnit::Percent,
                                                                     dir:    dir },

                Token::Dimension { value, ref unit, .. } => {
                    let value = f64::from(value);

                    match unit.as_ref () {
                        "em" => RsvgLength { length: value,
                                             unit:   LengthUnit::FontEm,
                                             dir:    dir },

                        "ex" => RsvgLength { length: value,
                                             unit:   LengthUnit::FontEx,
                                             dir:    dir },

                        "pt" => RsvgLength { length: value / POINTS_PER_INCH,
                                             unit:   LengthUnit::Inch,
                                             dir:    dir },

                        "in" => RsvgLength { length: value,
                                             unit:   LengthUnit::Inch,
                                             dir:    dir },

                        "cm" => RsvgLength { length: value / CM_PER_INCH,
                                             unit:   LengthUnit::Inch,
                                             dir:    dir },

                        "mm" => RsvgLength { length: value / MM_PER_INCH,
                                             unit:   LengthUnit::Inch,
                                             dir:    dir },

                        "pc" => RsvgLength { length: value / PICA_PER_INCH,
                                             unit:   LengthUnit::Inch,
                                             dir:    dir },

                        "px" => RsvgLength { length: value,
                                             unit:   LengthUnit::Default,
                                             dir:    dir },

                        _ => return Err (make_err ())
                    }
                },

                // FIXME: why are the following in Length?  They should be in FontSize
                Token::Ident (ref cow) => match cow.as_ref () {
                    "larger" => RsvgLength { length: 0.0,
                                             unit:   LengthUnit::RelativeLarger,
                                             dir:    dir },

                    "smaller" => RsvgLength { length: 0.0,
                                              unit:  LengthUnit::RelativeSmaller,
                                              dir:   dir },

                    "xx-small" |
                    "x-small" |
                    "small" |
                    "medium" |
                    "large" |
                    "x-large" |
                    "xx-large" => RsvgLength { length: compute_named_size (cow),
                                               unit:   LengthUnit::Inch,
                                               dir:    dir },

                    _ => return Err (make_err ())
                },

                _ => return Err (make_err ())
            }
        };

        Ok (length)
    }
}

fn viewport_percentage (x: f64, y: f64) -> f64 {
    /* https://www.w3.org/TR/SVG/coords.html#Units
     *
     * "For any other length value expressed as a percentage of the viewport, the
     * percentage is calculated as the specified percentage of
     * sqrt((actual-width)**2 + (actual-height)**2))/sqrt(2)."
     */
    (x * x + y * y).sqrt () / SQRT_2
}

// enum DashState {
//     None,
//     Inhereted,
//     DashArray(&str)
// }

// This does not handle "inherit" or "none" state, the calle should be responsible for that.
fn parse_length_list(s: &str) -> Result<Vec<RsvgLength>, AttributeError> {
    if s.is_empty() {
        return Err(AttributeError::Value("invalid syntax".into()));
    }

    // Values can be comma or whitespace separated.
    let mut dashes = s.split(',') // split at comma
        // split at whitespace
        .flat_map(|slice| slice.split_whitespace())
        // filter out empty strings("")
        .filter(|c| !c.is_empty())
        // parse it into an RsvgLength
        .map(|d| RsvgLength::parse(d.into(), LengthDir::Both))
        // collect into a Result<Vec<T>, E>.
        // it will short-circuit iteslf upon the first error encountered
        // like if you returned from a for-loop
        .collect::<Result<Vec<_>, _>>()?;

    // If the lenght of dashes is not even repeat them.
    // TODO: this could be done without extra allocations
    if !(dashes.len() % 2 == 0) {
        let len = dashes.len();
        dashes = dashes.into_iter().cycle().take(2 * len).collect();
    }

    // Get the sum of all values
    let sum: i64 = dashes.iter().map(|l| l.length as i64).sum();

    // If its 0 ignore the dash-array
    if sum == 0 {
        return Err(AttributeError::Value("invalid syntax".into()));
    }

    Ok(dashes)
}

#[test]
// TODO: add more test cases
fn test_parses_length_list() {
    // helper to cut down boilderplate
    let length_parse = |s| { RsvgLength::parse(s, LengthDir::Both).unwrap() };

    let expected =  vec![
       length_parse("1"),
       length_parse("2in"),
       length_parse("3"),
       length_parse("4%")
   ];

    let even =  vec![
       length_parse("1"),
       length_parse("2in"),
       length_parse("3"),
       length_parse("1"),
       length_parse("2in"),
       length_parse("3"),
   ];

   let sample_1 = vec![length_parse("10"), length_parse("6")];
   let sample_2 = vec![
       length_parse("5"),
       length_parse("5"),
       length_parse("20"),
       length_parse("5"),
       length_parse("5"),
       length_parse("20"),
   ];

   let sample_3 = vec![
       length_parse("10px"),
       length_parse("20px"),
       length_parse("20px"),
       length_parse("10px"),
       length_parse("20px"),
       length_parse("20px"),
   ];

   let sample_4 = vec![
       length_parse("25"),
       length_parse("5"),
       length_parse("5"),
       length_parse("5"),
   ];

   let sample_5 = vec![length_parse("3.1415926"), length_parse("8")];
   let sample_6 = vec![length_parse("5"), length_parse("3.14")];
   let sample_7 = vec![length_parse("2"), length_parse("2")];

    assert_eq!(parse_length_list("1 2in,3 4%").unwrap(), expected);
    assert_eq!(parse_length_list("1 2in,3").unwrap(), even);
    assert_eq!(parse_length_list("10,6").unwrap(), sample_1);
    assert_eq!(parse_length_list("5,5,20").unwrap(), sample_2);
    assert_eq!(parse_length_list("10px 20px 20px").unwrap(), sample_3);
    assert_eq!(parse_length_list("25  5 , 5 5").unwrap(), sample_4);
    assert_eq!(parse_length_list("3.1415926,8").unwrap(), sample_5);
    assert_eq!(parse_length_list("5, 3.14").unwrap(), sample_6);
    assert_eq!(parse_length_list("2").unwrap(), sample_7);

    // Empty dash_array
    assert!(parse_length_list("").is_err());
    assert!(parse_length_list("0").is_err());
    assert!(parse_length_list("15 -10 -5").is_err());
    // TODO:
    // syntax error dash_array
    // assert!(parse_length_list("syntax error").is_err());
    // another syntax error dash_array
    // assert!(parse_length_list("10px, syntax error").is_err());
    // another syntax error dash_array
    // assert!(parse_length_list("10 syntax error").is_err());
}

#[no_mangle]
pub extern fn rsvg_length_normalize (raw_length: *const RsvgLength, draw_ctx: *const RsvgDrawingCtx) -> f64 {
    assert! (!raw_length.is_null ());

    let length: &RsvgLength = unsafe { &*raw_length };

    length.normalize (draw_ctx)
}

#[no_mangle]
pub extern fn rsvg_length_hand_normalize (raw_length: *const RsvgLength,
                                          pixels_per_inch: f64,
                                          width_or_height: f64,
                                          font_size: f64) -> f64 {
    assert! (!raw_length.is_null ());

    let length: &RsvgLength = unsafe { &*raw_length };

    length.hand_normalize (pixels_per_inch, width_or_height, font_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default () {
        assert_eq! (RsvgLength::parse ("42", LengthDir::Horizontal),
                    Ok (RsvgLength::new(42.0, LengthUnit::Default, LengthDir::Horizontal)));

        assert_eq! (RsvgLength::parse ("-42px", LengthDir::Horizontal),
                    Ok (RsvgLength::new(-42.0, LengthUnit::Default, LengthDir::Horizontal)));
    }

    #[test]
    fn parses_percent () {
        assert_eq! (RsvgLength::parse ("50.0%", LengthDir::Horizontal),
                    Ok (RsvgLength::new(0.5, LengthUnit::Percent, LengthDir::Horizontal)));
    }

    #[test]
    fn parses_font_em () {
        assert_eq! (RsvgLength::parse ("22.5em", LengthDir::Vertical),
                    Ok (RsvgLength::new(22.5, LengthUnit::FontEm, LengthDir::Vertical)));
    }

    #[test]
    fn parses_font_ex () {
        assert_eq! (RsvgLength::parse ("22.5ex", LengthDir::Vertical),
                    Ok (RsvgLength::new(22.5, LengthUnit::FontEx, LengthDir::Vertical)));
    }

    #[test]
    fn parses_physical_units () {
        assert_eq! (RsvgLength::parse ("72pt", LengthDir::Both),
                    Ok (RsvgLength::new(1.0, LengthUnit::Inch, LengthDir::Both)));

        assert_eq! (RsvgLength::parse ("-22.5in", LengthDir::Both),
                    Ok (RsvgLength::new(-22.5, LengthUnit::Inch, LengthDir::Both)));

        assert_eq! (RsvgLength::parse ("-254cm", LengthDir::Both),
                    Ok (RsvgLength::new(-100.0, LengthUnit::Inch, LengthDir::Both)));

        assert_eq! (RsvgLength::parse ("254mm", LengthDir::Both),
                    Ok (RsvgLength::new(10.0, LengthUnit::Inch, LengthDir::Both)));

        assert_eq! (RsvgLength::parse ("60pc", LengthDir::Both),
                    Ok (RsvgLength::new(10.0, LengthUnit::Inch, LengthDir::Both)));
    }

    #[test]
    fn parses_relative_larger () {
        assert_eq! (RsvgLength::parse ("larger", LengthDir::Vertical),
                    Ok (RsvgLength::new(0.0, LengthUnit::RelativeLarger, LengthDir::Vertical)));
    }

    #[test]
    fn parses_relative_smaller () {
        assert_eq! (RsvgLength::parse ("smaller", LengthDir::Vertical),
                    Ok (RsvgLength::new(0.0, LengthUnit::RelativeSmaller, LengthDir::Vertical)));
    }

    #[test]
    fn parses_named_sizes () {
        let names = vec![ "xx-small",
                           "x-small",
                           "small",
                           "medium",
                           "large",
                           "x-large",
                           "xx-large" ];

        let mut previous_value: Option<f64> = None;

        // Just ensure that the values are progressively larger; don't
        // enforce a particular sequence.

        for name in names {
            let length = RsvgLength::parse (name, LengthDir::Both).unwrap ();

            assert_eq! (length.unit, LengthUnit::Inch);
            assert_eq! (length.dir, LengthDir::Both);

            if let Some (v) = previous_value {
                assert! (length.length > v);
            } else {
                previous_value = Some (length.length);
            }

        }
    }

    #[test]
    fn empty_length_yields_error () {
        assert! (is_parse_error (&RsvgLength::parse ("", LengthDir::Both)));
    }

    #[test]
    fn invalid_unit_yields_error () {
        assert! (is_parse_error (&RsvgLength::parse ("8furlong", LengthDir::Both)));
    }

    #[test]
    fn invalid_font_size_yields_error () {
        // FIXME: this is intended to test the (absence of) the "larger" et al values.
        // Since they really be in FontSize, not RsvgLength, we should remember
        // to move this test to that type later.
        assert! (is_parse_error (&RsvgLength::parse ("furlong", LengthDir::Both)));
    }

    #[test]
    fn check_nonnegative_works () {
        assert! (RsvgLength::parse ("0", LengthDir::Both).and_then (|l| l.check_nonnegative ()).is_ok ());
        assert! (RsvgLength::parse ("-10", LengthDir::Both).and_then (|l| l.check_nonnegative ()).is_err ());
    }
}

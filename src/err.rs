//! Typed LSTM errors. Match on [`Error::without_context`] to handle a failure;
//! use alternate display (`{error:#}`) or [`Error::chain`] for full diagnostics.

use std::{
    error::Error as StdError,
    fmt,
    num::{ParseIntError, TryFromIntError},
    path::PathBuf,
    sync::Arc,
};

/// Result returned by model loading, interpretation, drawing, and CLI operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Invalid L-system word or context-pattern syntax (formula syntax has its own type).
#[derive(Debug)]
pub enum SyntaxError {
    UnclosedArguments {
        input: String,
    },
    ExpectedArgument,
    InvalidArgumentName {
        name: String,
    },
    EmptyFormula {
        name: String,
    },
    ExpectedComma,
    MissingSymbol,
    ReservedSymbol {
        symbol: Arc<str>,
    },
    ExpectedSingleSymbol {
        input: String,
    },
    UnexpectedContextSuffix {
        suffix: String,
    },
    ContextNestingLimit {
        limit: usize,
    },
    MissingClosingParenthesis,
    InvalidContextAtom {
        symbol: Arc<str>,
    },
    RepeatedAnchor,
    InvalidRepetitionBounds,
    InvalidRepetitionCount {
        input: String,
        source: ParseIntError,
    },
    InvalidPredecessor {
        input: String,
    },
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnclosedArguments { input } => write!(f, "Unclosed argument block: {input}"),
            Self::ExpectedArgument => f.write_str("Expected argument name: expression"),
            Self::InvalidArgumentName { name } => write!(f, "Invalid argument name {name}"),
            Self::EmptyFormula { name } => write!(f, "Empty formula for {name}"),
            Self::ExpectedComma => f.write_str("Expected comma after formula"),
            Self::MissingSymbol => f.write_str("Missing symbol"),
            Self::ReservedSymbol { symbol } => write!(f, "Reserved symbol {symbol}"),
            Self::ExpectedSingleSymbol { input } => write!(f, "Expected one symbol in {input}"),
            Self::UnexpectedContextSuffix { suffix } => {
                write!(f, "Unexpected context suffix {suffix}")
            }
            Self::ContextNestingLimit { limit } => {
                write!(f, "Context nesting exceeded {limit} groups")
            }
            Self::MissingClosingParenthesis => f.write_str("Missing )"),
            Self::InvalidContextAtom { symbol } => write!(f, "Invalid context atom {symbol}"),
            Self::RepeatedAnchor => f.write_str("Repeated anchor"),
            Self::InvalidRepetitionBounds => f.write_str("Invalid repetition bounds"),
            Self::InvalidRepetitionCount { input, .. } => {
                write!(f, "Invalid repetition count: {input}")
            }
            Self::InvalidPredecessor { input } => write!(f, "Invalid predecessor {input}"),
        }
    }
}
impl StdError for SyntaxError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::InvalidRepetitionCount { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Structured validation failures and typed external errors, with optional context.
#[derive(Debug)]
pub enum Error {
    Syntax(SyntaxError),
    NativeSyntax {
        line: usize,
        column: usize,
        message: String,
    },
    Formula(Box<formulas::Error>),
    Json5(Box<json5::Error>),
    Io(std::io::Error),
    Window(minifb::Error),
    Gif(gif::EncodingError),
    Png(png::EncodingError),
    Persist(Box<tempfile::PersistError>),
    InvalidRuleWeight {
        weight: f64,
    },
    InvalidTotalWeight {
        total: f64,
    },
    ExpectedBoolean {
        actual: String,
    },
    InvalidPredicate,
    UnclosedBranch,
    DrawingRecursion {
        limit: usize,
        path: String,
    },
    InvalidCanvasDimensions {
        width: u32,
        height: u32,
    },
    InvalidUnsignedSetting {
        field: &'static str,
        value: i64,
        source: TryFromIntError,
    },
    InvalidFrameRate {
        fps: f64,
        reason: &'static str,
    },
    UnsupportedOutput {
        extension: String,
    },
    OutputOverwritesModel {
        path: PathBuf,
    },
    TracePathConflict {
        path: PathBuf,
    },
    UnboundedIteration,
    GenerationOverflow,
    InvalidTraceRange {
        input: String,
        source: Option<ParseIntError>,
    },
    InvalidArgument {
        field: String,
        expected: String,
        got: EvalValue,
    },
    InvalidToken(String),
    ExprForTok(String),
    InvalidTokenExpression(String),
    InvalidJoker,
    TrailingInput(String),
    Context {
        message: String,
        source: Box<Error>,
    },
}

impl Error {
    /// Adds diagnostic context without discarding the original variant or source.
    pub fn context(self, message: impl Into<String>) -> Self {
        Self::Context {
            message: message.into(),
            source: Box::new(self),
        }
    }
    /// Returns the underlying LSTM variant beneath diagnostic context wrappers.
    pub fn without_context(&self) -> &Self {
        let mut error = self;
        while let Self::Context { source, .. } = error {
            error = source;
        }
        error
    }
    /// Iterates over this error and its sources, including external-library causes.
    pub fn chain(&self) -> impl Iterator<Item = &(dyn StdError + 'static)> {
        let mut next: Option<&(dyn StdError + 'static)> = Some(self);
        std::iter::from_fn(move || {
            let error = next?;
            next = error.source();
            Some(error)
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(_) => f.write_str("Invalid L-system syntax")?,
            Self::NativeSyntax {
                line,
                column,
                message,
            } => write!(f, "Native syntax at {line}:{column}: {message}")?,
            Self::Formula(_) => f.write_str("Formula evaluation failed")?,
            Self::Json5(_) => f.write_str("Invalid JSON5 model")?,
            Self::Io(_) => f.write_str("I/O operation failed")?,
            Self::Window(_) => f.write_str("Display operation failed")?,
            Self::Gif(_) => f.write_str("GIF encoding failed")?,
            Self::Png(_) => f.write_str("PNG encoding failed")?,
            Self::Persist(_) => f.write_str("Could not publish output file")?,
            Self::InvalidRuleWeight { weight } => write!(f, "Invalid rule weight: {weight}")?,
            Self::InvalidTotalWeight { total } => write!(f, "Total weight is not finite: {total}")?,
            Self::ExpectedBoolean { actual } => write!(f, "Expected boolean, got {actual}")?,
            Self::InvalidPredicate => f.write_str("Predicate must accept one argument")?,
            Self::UnclosedBranch => f.write_str("Unclosed branch in drawing context")?,
            Self::DrawingRecursion { limit, path } => {
                write!(f, "Drawing recursion exceeded {limit} rules: {path}")?
            }
            Self::InvalidCanvasDimensions { width, height } => {
                write!(f, "Invalid canvas dimensions: {width}x{height}")?
            }
            Self::InvalidUnsignedSetting { field, value, .. } => write!(
                f,
                "Invalid {field}: {value} is outside the unsigned 32-bit range"
            )?,
            Self::InvalidFrameRate { fps, reason } => {
                write!(f, "Invalid frame rate {fps}: {reason}")?
            }
            Self::UnsupportedOutput { extension } => {
                write!(f, "Output extension must be .png or .gif, got .{extension}")?
            }
            Self::OutputOverwritesModel { path } => write!(
                f,
                "Output would overwrite the input model: {}",
                path.display()
            )?,
            Self::TracePathConflict { path } => {
                write!(f, "Trace needs a separate path: {}", path.display())?
            }
            Self::UnboundedIteration => {
                f.write_str("Unbounded iteration requires live animation")?
            }
            Self::InvalidArgument {
                field,
                expected,
                got,
            } => write!(
                f,
                "Invalid argument for {field}: expected {expected}, got {} (type {})",
                got.repr(),
                got.type_name()
            )?,
            Self::GenerationOverflow => f.write_str("Generation overflow")?,
            Self::InvalidTraceRange { input, .. } => write!(
                f,
                "Expected N or START:END with 0 <= START <= END, got {input}"
            )?,
            Self::InvalidToken(msg) => write!(f, "Invalid token: {msg}")?,
            Self::ExprForTok(expr) => write!(f, "Expected a token, got an expression: {expr}")?,
            Self::InvalidTokenExpression(expr) => write!(f, "Invalid token expression: {expr}")?,
            Self::InvalidJoker => write!(f, "Invalid joker")?,
            Self::TrailingInput(expr) => write!(f, "Trailing input after expression: {expr}")?,
            Self::Context { message, .. } => f.write_str(message)?,
        }
        if f.alternate() {
            // Walk the chain explicitly: external error types need not implement
            // alternate formatting, and LSTM context must not duplicate sources.
            for error in self.chain().skip(1) {
                write!(f, ": {error}")?;
            }
        }
        Ok(())
    }
}
impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Syntax(source) => Some(source),
            Self::Formula(source) => Some(source.as_ref()),
            Self::Json5(source) => Some(source.as_ref()),
            Self::Io(source) => Some(source),
            Self::Window(source) => Some(source),
            Self::Gif(source) => Some(source),
            Self::Png(source) => Some(source),
            Self::Persist(source) => Some(source.as_ref()),
            Self::InvalidTraceRange { source, .. } => source.as_ref().map(|e| e as &dyn StdError),
            Self::InvalidUnsignedSetting { source, .. } => Some(source),
            Self::Context { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

macro_rules! from_error {
    ($source:ty, $variant:ident) => {
        impl From<$source> for Error {
            fn from(source: $source) -> Self {
                Self::$variant(source.into())
            }
        }
    };
}
from_error!(SyntaxError, Syntax);
from_error!(formulas::Error, Formula);
from_error!(json5::Error, Json5);
from_error!(std::io::Error, Io);
from_error!(minifb::Error, Window);
from_error!(gif::EncodingError, Gif);
from_error!(png::EncodingError, Png);
from_error!(tempfile::PersistError, Persist);

/// Adds context while retaining a concrete error and its source chain.
pub trait ResultContext<T> {
    fn context(self, message: impl Into<String>) -> Result<T>;
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T>;
}
impl<T, E: Into<Error>> ResultContext<T> for std::result::Result<T, E> {
    fn context(self, message: impl Into<String>) -> Result<T> {
        self.map_err(|error| error.into().context(message))
    }
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|error| error.into().context(message()))
    }
}

// Validation sites must supply a typed error, never an unclassified message.
macro_rules! ensure {
    ($condition:expr, $error:expr $(,)?) => {{
        let valid = $condition;
        if !valid {
            return Err($error.into());
        }
    }};
}
pub(crate) use ensure;
use formulas::EvalValue;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_preserves_typed_formula_and_io_sources() {
        let formula = formulas::Error::UnknownVariable {
            name: "missing".into(),
            available: "".into(),
        }
        .context("evaluating expression");
        let error = Error::from(formula)
            .context("global length")
            .context("loading model");
        let Error::Formula(source) = error.without_context() else {
            panic!("formula error was erased")
        };
        assert!(
            matches!(source.without_context(), formulas::Error::UnknownVariable { name, .. } if name == "missing")
        );
        assert!(
            error
                .chain()
                .any(|source| source.downcast_ref::<formulas::Error>().is_some())
        );
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.starts_with(
            "loading model: global length: Formula evaluation failed: evaluating expression:"
        ));
        assert!(diagnostic.contains("missing"));

        let error = Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ))
        .context("writing trace");
        let Error::Io(source) = error.without_context() else {
            panic!("I/O error was erased")
        };
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            format!("{error:#}"),
            "writing trace: I/O operation failed: denied"
        );
    }

    #[test]
    fn syntax_diagnostics_do_not_repeat_the_cause() {
        let error = Error::from(SyntaxError::ReservedSymbol {
            symbol: Arc::from("+"),
        })
        .context("axiom");
        assert!(match error.without_context() {
            Error::Syntax(SyntaxError::ReservedSymbol { symbol: s }) if s.as_ref() == "+" => true,
            _ => false,
        });
        assert_eq!(
            format!("{error:#}"),
            "axiom: Invalid L-system syntax: Reserved symbol +"
        );
    }
}

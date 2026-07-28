use chumsky::{
    error::Error,
    input::Stream,
    label::LabelError,
    pratt::{Associativity, infix, prefix},
    prelude::*,
    util::Maybe,
};
use proc_macro2::{Delimiter, Span, TokenStream, TokenTree, token_stream::IntoIter};
use quote::{ToTokens, quote};
use syn::{Ident, Lit, LitInt, LitStr, Token};

// --- ast ---

#[derive(Clone, Debug)]
struct SelectClause {
    projection_clause: Vec<ProjectionBindingClause>,
    from_clause: Vec<FromBindingClause>,
    where_clause: ExpressionClause,
}

#[derive(Clone, Debug)]
struct ProjectionBindingClause {
    expr: ExpressionClause,
    alias: Option<Ident>,
}

#[derive(Clone, Debug)]
struct FromBindingClause {
    table: Ident,
    alias: Option<Ident>,
}

#[derive(Clone, Debug)]
enum ExpressionClause {
    Basic(BasicExpressionClause),
    UnaryPrefixOperator(UnaryPrefixOperatorClause),
    BinaryOperator(BinaryOperatorClause),
}

#[derive(Clone, Debug)]
enum BasicExpressionClause {
    Null,
    String(LitStr),
    Integer(LitInt),
    FieldReference(Ident, Ident),
    Expression(Box<ExpressionClause>),
}

#[derive(Clone, Debug)]
enum UnaryPrefixOperator {
    Not,
}

#[derive(Clone, Debug)]
struct UnaryPrefixOperatorClause {
    operator: UnaryPrefixOperator,
    target: Box<ExpressionClause>,
}

#[derive(Clone, Debug)]
enum BinaryOperator {
    Eq,
    Lt,
    Gt,
}

#[derive(Clone, Debug)]
struct BinaryOperatorClause {
    operator: BinaryOperator,
    left: Box<ExpressionClause>,
    right: Box<ExpressionClause>,
}

// --- parser ---

#[derive(Debug)]
struct Messaged<'src> {
    pub message: String,
    pub simple: Simple<'src, TokenTree, SimpleSpan>,
}

impl<'src> Messaged<'src> {
    pub fn new(
        message: impl Into<String>,
        simple: Simple<'src, TokenTree, SimpleSpan>,
    ) -> Messaged<'src> {
        Self {
            message: message.into(),
            simple,
        }
    }
}

impl<'src> Error<'src, Stream<IntoIter>> for Messaged<'src> {}

impl<'src, L> LabelError<'src, Stream<IntoIter>, L> for Messaged<'src> {
    fn expected_found<E: IntoIterator<Item = L>>(
        _expected: E,
        found: Option<chumsky::util::MaybeRef<'src, <Stream<IntoIter> as Input>::Token>>,
        span: <Stream<IntoIter> as Input>::Span,
    ) -> Self {
        Self::new("".to_string(), Simple::new(found, span))
    }
}

macro_rules! define_parser {
    ($var: ident, $keyword: literal, $message: literal) => {
        let $var = any().try_map(|x: TokenTree, s: SimpleSpan| {
            let parsed = syn::parse2::<Ident>(x.to_token_stream());
            match parsed {
                Ok(p) if p.to_string() == $keyword => Ok(x),
                _ => Err(Messaged::new($message, Simple::new(Some(Maybe::Val(x)), s))),
            }
        });
    };

    ($var: ident, $keyword: tt, $message: literal) => {
        let $var = any().try_map(|x: TokenTree, s: SimpleSpan| {
            let parsed = syn::parse2::<Token![$keyword]>(x.to_token_stream());
            match parsed {
                Ok(_) => Ok(x),
                _ => Err(Messaged::new($message, Simple::new(Some(Maybe::Val(x)), s))),
            }
        });
    };

    ($var: ident, $parsing: ty, $ident: ident => $body: expr, $message: literal) => {
        let $var = any().try_map(|x: TokenTree, s: SimpleSpan| {
            let parsed = syn::parse2::<$parsing>(x.to_token_stream());
            match parsed {
                Ok($ident) => $body,
                Err(_) => Err(Messaged::new($message, Simple::new(Some(Maybe::Val(x)), s))),
            }
        });
    };

    ($var: ident, $parsing: ty, $pattern: pat => $body: expr, $message: literal) => {
        let $var = any().try_map(|x: TokenTree, s: SimpleSpan| {
            let parsed = syn::parse2::<$parsing>(x.to_token_stream());
            match parsed {
                Ok(parsed) => match parsed {
                    $pattern => $body,
                    _ => Err(Messaged::new($message, Simple::new(Some(Maybe::Val(x)), s))),
                },
                Err(_) => Err(Messaged::new($message, Simple::new(Some(Maybe::Val(x)), s))),
            }
        });
    };
}

fn parser() -> impl Parser<'static, Stream<IntoIter>, SelectClause, extra::Err<Messaged<'static>>> {
    define_parser!(ident, Ident, p => Ok(p), "expected identifier");
    define_parser!(string_lit, Lit, Lit::Str(p) => Ok(p), "expected string");
    define_parser!(int_lit, Lit, Lit::Int(p) => Ok(p), "expected integer");
    define_parser!(select_keyword, "select", "expected 'select' keyword");
    define_parser!(from_keyword, "from", "expected 'from' keyword");
    define_parser!(not_keyword, "not", "expected 'not' keyword");
    define_parser!(null_keyword, "null", "expected 'null' keyword");
    define_parser!(as_keyword, as, "expected 'as' keyword");
    define_parser!(where_keyword, where, "expected 'where' keyword");
    define_parser!(dot, ., "expected '.' symbol");
    define_parser!(comma, ,, "expected ',' symbol");
    define_parser!(eq, =, "expected '=' symbol");
    define_parser!(lt, <, "expected '<' symbol");
    define_parser!(gt, >, "expected '>' symbol");

    let field_reference = ident.then_ignore(dot).then(ident);

    let basic_expr = choice((
        null_keyword.map(|_| ExpressionClause::Basic(BasicExpressionClause::Null)),
        string_lit.map(|x| ExpressionClause::Basic(BasicExpressionClause::String(x))),
        int_lit.map(|x| ExpressionClause::Basic(BasicExpressionClause::Integer(x))),
        field_reference
            .map(|x| ExpressionClause::Basic(BasicExpressionClause::FieldReference(x.0, x.1))),
    ));

    let parens = custom(|input| {
        let before = input.cursor();
        if let Some(next) = input.next() {
            match next {
                TokenTree::Group(group) if matches!(group.delimiter(), Delimiter::Parenthesis) => {
                    Ok(Stream::from_iter(group.stream().into_iter()))
                }
                _ => Err(Messaged::new(
                    "expression must be in parens",
                    Simple::new(Some(Maybe::Val(next)), input.span_since(&before)),
                )),
            }
        } else {
            Err(Messaged::new(
                "not found next tokens",
                Simple::new(None, input.span_since(&before)),
            ))
        }
    });

    let expression = recursive(|rec| {
        let atom = basic_expr.or(rec
            .nested_in(parens)
            .map(|x| ExpressionClause::Basic(BasicExpressionClause::Expression(Box::new(x)))));
        atom.pratt((
            prefix(0, not_keyword, |_, r: ExpressionClause, _| {
                ExpressionClause::UnaryPrefixOperator(UnaryPrefixOperatorClause {
                    operator: UnaryPrefixOperator::Not,
                    target: Box::new(r),
                })
            }),
            infix(Associativity::Left(2), eq, |l, _, r, _| {
                ExpressionClause::BinaryOperator(BinaryOperatorClause {
                    operator: BinaryOperator::Eq,
                    left: Box::new(l),
                    right: Box::new(r),
                })
            }),
            infix(Associativity::Left(1), gt, |l, _, r, _| {
                ExpressionClause::BinaryOperator(BinaryOperatorClause {
                    operator: BinaryOperator::Gt,
                    left: Box::new(l),
                    right: Box::new(r),
                })
            }),
            infix(Associativity::Left(1), lt, |l, _, r, _| {
                ExpressionClause::BinaryOperator(BinaryOperatorClause {
                    operator: BinaryOperator::Lt,
                    left: Box::new(l),
                    right: Box::new(r),
                })
            }),
        ))
    });

    let projection = expression
        .clone()
        .then(as_keyword.ignore_then(ident).or_not())
        .separated_by(comma)
        .allow_trailing()
        .collect::<Vec<_>>()
        .map(|xs| {
            xs.into_iter()
                .map(|x| ProjectionBindingClause {
                    expr: x.0,
                    alias: x.1,
                })
                .collect::<Vec<_>>()
        });

    let from = ident
        .then(as_keyword.ignore_then(ident).or_not())
        .separated_by(comma)
        .allow_trailing()
        .collect::<Vec<_>>()
        .map(|xs| {
            xs.into_iter()
                .map(|x| FromBindingClause {
                    table: x.0,
                    alias: x.1,
                })
                .collect::<Vec<_>>()
        });

    select_keyword
        .ignore_then(projection)
        .then_ignore(from_keyword)
        .then(from)
        .then_ignore(where_keyword)
        .then(expression)
        .map(|x| SelectClause {
            projection_clause: x.0.0,
            from_clause: x.0.1,
            where_clause: x.1,
        })
}

pub fn make_select(body: TokenStream) -> TokenStream {
    let stream = Stream::from_iter(body.into_iter());
    let select = parser().parse(stream).into_result();
    let Ok(select) = select else {
        if let Err(errs) = select {
            let mut error: Option<syn::Error> = None;
            for err in errs {
                if let Some(x) = err.simple.found() {
                    if let Some(ref mut error) = error {
                        error.combine(syn::Error::new(x.span(), err.message));
                    } else {
                        error = Some(syn::Error::new(x.span(), err.message));
                    }
                } else {
                    if let Some(ref mut error) = error {
                        error.combine(syn::Error::new(Span::call_site(), err.message));
                    } else {
                        error = Some(syn::Error::new(Span::call_site(), err.message));
                    }
                }
            }
            return error.unwrap().into_compile_error();
        }
        unreachable!()
    };
    let select_str = format!("{:?}", select);
    quote! {#select_str}
}

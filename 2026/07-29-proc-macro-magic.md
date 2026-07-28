# Rust Procedural Macros Magic

Как в целом происходит парсинг языка? На вход подаётся строка текста, затем специальный *lexer* разбивает строку на лексемы (минимальные синтаксические единицы текста), а после *parser* обходит этот список лексем, составляя синтаксическое дерево. Синтаксическое дерево же, в свою очередь, может быть использовано для анализа текста, исполнения команд, записанных в этом тексте и т.п.

Предположим, что мы решаем задачу построения парсера SQL на Rust. Например, как в `sqlx`:

```rust
sqlx::query(
    r#"
    select table.column
    from table
    where table.column is not null 
    "#
)
```

Это просто функция, принимающая на вход строку. Внутри себя `sqlx` её парсит и преобразует в запрос к целевой базе данных. Всё хорошо, но как на этапе компиляции программы быть уверенным, что все составленные запросы грамматически верны? Парсер `sqlx` отработает в рантайме по месту вызова запроса, поэтому невозможно быть уверенным в корректности запросов.

В том же `sqlx` есть макрос `query!` ([тут пруф](https://docs.rs/sqlx/latest/sqlx/macro.query.html)), который позволяет на этапе компиляции сделать запрос к базе данных и проверить корректность записанного запроса. Для этого при компиляции необходимо указать переменную окружения `DATABASE_URL` или пользоваться предсохранёнными `.sqlx`-файлами (см. Offline Mode по той же ссылке). Это неплохое решение, но всё-таки есть альтернатива.

## У этого решения есть критический недостаток (с)

Что если хочется записать запросы так, чтобы компилятор проверял корректность грамматики SQL и в то же время подсвечивал, в каком месте ошибка? В Rust это возможно, если использовать процедурные макросы довольно необычным образом. Вместо того, чтобы подавать в макрос на вход запрос как строку, мы передадим туда непосредственно Rust-овые лексемы, которые будут выглядеть как SQL-запрос. Примерно так:

```rust
use x_macro::select;

fn main() {
    let q = select! {
        select table.column
        from table
        where not table.column = null 
    };
    println!("{}", q);
}
```

Этот код расположен здесь: [`./projects/rust-proc-macro-magic/x-run/src/main.rs`](./projects/rust-proc-macro-magic/x-run/src/main.rs). В качестве иллюстрации подхода я только лишь реализую парсер, без анализатора синтаксического дерева. Также в ходе реализации я не буду прописывать обработчики ошибок в большом количестве дабы сократить количество кода.

Итак, нам нужно два крейта: [`x-macro`](./projects/rust-proc-macro-magic/x-macro) и [`x-macro-lib`](./projects/rust-proc-macro-magic/x-macro-lib). `x-macro` представляет собой экспортный крейт с процедурными макросами - в Rust необходимо выделять процедурные макросы в отдельные крейты и добавлять в `Cargo.toml` секцию:

```toml
[lib]
proc-macro = true
```

Сам контент крейта минимален, он включает в себя основную зависимость: `x-macro-lib` - и предоставляет единственный макрос:

```rust
use proc_macro::TokenStream;

#[proc_macro]
pub fn select(body: TokenStream) -> TokenStream {
    x_macro_lib::make_select(body.into()).into()
}
```

Входной аргумент `body: TokenStream` - это как раз последовательность лексем Rust, подающаяся нам компилятором на вход.

Вся работа по парсингу будет происходить в `x-macro-lib`. Разберём зависимости крейта:

```toml
[dependencies]
proc-macro2 = "1"
syn = { version = "2", features = ["full", "extra-traits"] }
quote = "1"
chumsky = { version = "0.13", features = ["pratt"] }
```

- `proc-macro2` - библиотека, предоставляющая обёртку над обычным `TokenStream`, удобную для парсинга
- `syn` - библиотека парсинга `TokenStream`-ов, из неё нам нужен будет лексер
- `quote` - библиотека генерации `TokenStream`-ов, она нужна нам, так как на выход из процедурного макроса нам надо выдать валидный Rust-код, то есть тоже `TokenStream`
- `chumsky` - библиотека парсинга общего назначения, которую мы будем использовать в качестве примера

Итак, самое важное, что нужно сделать ([исходник](./projects/rust-proc-macro-magic/x-macro-lib/src/lib.rs#L275)) - это разобраться с тем, как преобразовать `TokenStream` во что-то, что будет поддерживать `chumsky`. В `chumsky` уже есть абстракция, позволяющая работать с произвольными стримами токенов, нужно лишь использовать метод, имеющийся у `proc_macro2::TokenStream`:

```rust
let stream: chumsky::Stream<proc_macro2::token_stream::IntoIter> = 
    Stream::from_iter(body.into_iter());
```

Этот стрим и будет нашим списком лексем для парсера. Сам вызов парсера будет сводиться к простейшему:

```rust
let select = parser().parse(stream).into_result();
let Ok(select) = select else {
    // обработка ошибок
}
```

В библиотеке `syn` есть структура `syn::Error`, у которой есть замечательный метод `into_compile_error()`, возвращающий `TokenStream`. Этот `TokenStream` не будет содержать результирующего кода, но вызовет корректно отформатированную ошибку компиляции с привязкой ошибки к конкретным лексемам, где эта ошибка и произошла. Вся магия происходит [здесь](./projects/rust-proc-macro-magic/x-macro-lib/src/lib.rs#L279-L297):

```rust
// тип ошибки парсинга
#[derive(Debug)]
struct Messaged<'src> {
    pub message: String,
    pub simple: Simple<'src, TokenTree, SimpleSpan>,
}

// ---

if let Err(errs) = select {
    let mut error: Option<syn::Error> = None;
    for err in errs { // err: Messaged<'_>
        if let Some(x) = err.simple.found() { // если есть конкретный токен, на котором свалились
            if let Some(ref mut error) = error {
                // комбинируем предыдущую ошибку с новой, чтобы отображать все
                // найденные ошибки сразу
                error.combine(syn::Error::new(x.span(), err.message));
            } else {
                error = Some(syn::Error::new(x.span(), err.message));
            }
        } else {
            if let Some(ref mut error) = error {
                // fallback
                error.combine(syn::Error::new(Span::call_site(), err.message));
            } else {
                error = Some(syn::Error::new(Span::call_site(), err.message));
            }
        }
    }
    return error.unwrap().into_compile_error();
}
```

## А как парсить?

На этом этапе необъяснённой осталась только функция `parser()`. Фактически необходимо записать следующее:

```rust
fn parser() -> impl Parser<
    'static,
    Stream<IntoIter>, // тот самый входной стрим лексем
    SelectClause, // корень синтаксического дерева
    extra::Err<Messaged<'static>> // наш обёрнутый тип ошибки
> {
    // реализация парсинга
}
```

Теперь осталось немногое - лишь написать грамматические правила в синтаксисе `chumsky`. В целом, это делается абсолютно аналогично любому другому примеру использования `chumsky`, кроме нескольких интересных случаев.

Первый - это парсинг примитивов и ключевых слов. Например, чтобы корректно распарсить ключевое слово `select`, нужно выполнить довольно нетривиальный код:

```rust
// any() просто захватывает следующую лексему из стрима, безотносительно её типа
let select_keyword = any().try_map(|x: TokenTree, s: SimpleSpan| { // стрим оперирует типом TokenTree
    // а syn ожидает в качестве входа именно TokenStream
    // поэтому мы конвертируем TokenTree обратно в TokenStream и парсим через syn ещё раз
    // не очень эффективно, но работает
    let parsed = syn::parse2::<Ident>(x.to_token_stream());
    // проверяем, что Rust-идентификатор есть 'select' и выбрасываем ошибку в противном случае
    match parsed {
        Ok(p) if p.to_string() == "select" => Ok(x),
        _ => Err(Messaged::new("expected 'select' keyword", Simple::new(Some(Maybe::Val(x)), s))),
    }
});
```

Данный код шаблонный для всех ключевых слов, поэтому в примере я вынес декларацию примитивов в отдельный (уже не процедурный) макрос ([здесь](./projects/rust-proc-macro-magic/x-macro-lib/src/lib.rs#L107)).

Второй интересный момент - это то, как Rust-лексер обрабатывает скобки (и в целом "вложенные" конструкции). Лексер не даёт последовательность символов как она есть. Вместо этого `TokenTree` становится вариантом `TokenTree::Group`, который содержит собственный стрим лексем. Это обстоятельство радикально отличает наш случай от обычного. Обычно текст парсится в цепочку лексем, а не в самостоятельное дерево (название `TokenTree` как бы намекает).

Но и это неудобство можно обойти с помощью функции `chumsky::custom`:

```rust
let parens = custom(|input| {
    let before = input.cursor(); // сохраняем начальную позицию на случай ошибки
    if let Some(next) = input.next() { // берём следующий токен
        match next {
            // ожидаем нашу вложенную group, которая представляется стримом лексем
            TokenTree::Group(group) if matches!(group.delimiter(), Delimiter::Parenthesis) => {
                // возвращаем в качестве результата стрим лексем, расположенных внутри скобок
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
```

Хорошо, что с этой конструкцией делать? А оказывается, что она идеально комбинируется с методом `nested_in` из `chumsky`. Выглядеть это будет так:

```rust
// expression: impl Parser<...>
let parens_expression = expression.nested_in(parens);
```

И читается эта конструкция именно так, как она и работает: `parens_expression` - это обычный `expression` помещённый в скобки `parens`. В примере это используется [здесь](./projects/rust-proc-macro-magic/x-macro-lib/src/lib.rs#L199). Правда, там немного сложный момент с рекурсией и парсингом выражений по алгоритму Пратта, поэтому здесь в тексте я вынес более понятный пример.

## Заключение

С помощью такого подхода можно реализовывать собственные DSL (Domain Specific Language) в синтаксисе Rust с проверкой/подсветкой ошибок в compile time. Звучит неплохо, по моему мнению. Если не просто выводить ошибки как message-строки, а использовать что-нибудь в духе [`miette`](https://docs.rs/miette/latest/miette/), то пользовательский опыт использования DSL ничем не будет отличаться от пользовательского опыта при написании кода. Единственное, не знаю пока, как подсказки генерировать в макросах - возможно, и никак.
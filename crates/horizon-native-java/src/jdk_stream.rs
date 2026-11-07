//! `java.util.Comparator` values and the stream stages config and service
//! code reaches beyond `map`/`filter`/`collect`.
//!
//! The runtime's streams are eager: a stream marker owns the list of its
//! elements (`Runtime::streams`), and every stage answers a new marker. This
//! module adds the sources and stages real jars stopped on:
//!
//! * `IntStream.range`/`rangeClosed`/`of` and the `LongStream` ranges, with
//!   `mapToObj`, `mapToInt`/`Long`/`Double`, `boxed` and `sum` (Xaero's
//!   config options enumerate their indices with
//!   `IntStream.rangeClosed(min, max).mapToObj(...)`).
//! * `sorted`, `max`, `min`, `findFirst`, `findAny` and `reduce` (rrls picks
//!   its highest-priority service with `stream.max(COMPARATOR)`).
//!
//! A comparator is a marker in the runtime's string vocabulary, built by the
//! host from `Comparator`'s factories and combinators and evaluated here,
//! where jar callbacks can run:
//!
//! * `@cmp:natural`: `Comparator.naturalOrder()`.
//! * `@cmp:reverse:<c>`: `c.reversed()`, `Comparator.reverseOrder()`.
//! * `@cmp:key:<f>`: `Comparator.comparing(f)` and the `comparingInt`,
//!   `comparingLong`, `comparingDouble` forms.
//! * `@cmp:keycmp:<f>,<c>`: `Comparator.comparing(f, c)`.
//! * `@cmp:then:<a>,<b>`: `a.thenComparing(b)`.
//! * `@cmp:entry-key` / `@cmp:entry-value`: `Map.Entry.comparingByKey()` /
//!   `comparingByValue()`.
//!
//! Operands are escaped capture markers (`value_marker`), so a jar lambda's
//! `@indy:...` marker nests unchanged. Any other comparator value (a jar
//! lambda, method reference or `Comparator` object) is called through the
//! callback path. Natural order covers the boxed numbers, characters,
//! booleans and strings (`String.compareTo` over UTF-16 units); anything else
//! is refused with a named reason rather than ordered arbitrarily.

use crate::jdk_model::MAX_SPLITERATOR_ELEMENTS;
use crate::{MethodRef, NativeError, Value, expect_ref};

/// Prefix of a modelled `Comparator` marker.
const COMPARATOR_PREFIX: &str = "@cmp:";

fn comparator(body: &str) -> Value {
    Value::Ref(Some(format!("{COMPARATOR_PREFIX}{body}")))
}

fn escaped(values: &[&Value]) -> String {
    values
        .iter()
        .map(|value| super::value_marker(value))
        .collect::<Vec<_>>()
        .join(",")
}

/// `Comparator`'s static factories and `Map.Entry`'s comparators, answered by
/// the host as comparator markers.
pub(crate) fn comparator_static(r: &MethodRef, args: &[Value]) -> Option<Value> {
    let first = || args.first().unwrap_or(&Value::Ref(None));
    match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
        ("java/util/Comparator", "naturalOrder", "()Ljava/util/Comparator;") => {
            Some(comparator("natural"))
        }
        (
            "java/util/Comparator" | "java/util/Collections",
            "reverseOrder",
            "()Ljava/util/Comparator;",
        ) => Some(comparator(&format!(
            "reverse:{}",
            escaped(&[&comparator("natural")])
        ))),
        (
            "java/util/Collections",
            "reverseOrder",
            "(Ljava/util/Comparator;)Ljava/util/Comparator;",
        ) => Some(comparator(&format!("reverse:{}", escaped(&[first()])))),
        (
            "java/util/Comparator",
            "comparing" | "comparingInt" | "comparingLong" | "comparingDouble",
            descriptor,
        ) if descriptor.ends_with(")Ljava/util/Comparator;") => match args {
            [key] => Some(comparator(&format!("key:{}", escaped(&[key])))),
            [key, order] => Some(comparator(&format!("keycmp:{}", escaped(&[key, order])))),
            _ => None,
        },
        ("java/util/Map$Entry", "comparingByKey", "()Ljava/util/Comparator;") => {
            Some(comparator("entry-key"))
        }
        ("java/util/Map$Entry", "comparingByValue", "()Ljava/util/Comparator;") => {
            Some(comparator("entry-value"))
        }
        _ => None,
    }
}

/// `reversed()` and `thenComparing*` on any comparator value: a modelled
/// marker or a jar lambda. Answered by the host, since combining needs no
/// callback to run.
pub(crate) fn comparator_instance(
    r: &MethodRef,
    receiver: &Value,
    args: &[Value],
) -> Option<Value> {
    if r.owner != "java/util/Comparator" || matches!(receiver, Value::Ref(None)) {
        return None;
    }
    match (r.name.as_str(), r.descriptor.as_str()) {
        ("reversed", "()Ljava/util/Comparator;") => {
            Some(comparator(&format!("reverse:{}", escaped(&[receiver]))))
        }
        ("thenComparing", "(Ljava/util/Comparator;)Ljava/util/Comparator;") => {
            let other = args.first()?;
            Some(comparator(&format!("then:{}", escaped(&[receiver, other]))))
        }
        (
            "thenComparing" | "thenComparingInt" | "thenComparingLong" | "thenComparingDouble",
            descriptor,
        ) if descriptor.ends_with(")Ljava/util/Comparator;") => {
            let key = match args {
                [key] => comparator(&format!("key:{}", escaped(&[key]))),
                [key, order] => comparator(&format!("keycmp:{}", escaped(&[key, order]))),
                _ => return None,
            };
            Some(comparator(&format!("then:{}", escaped(&[receiver, &key]))))
        }
        _ => None,
    }
}

/// An integral boxed or primitive value, widened to `long`.
fn integral(value: &Value) -> Option<i64> {
    match value {
        Value::Int(value) => Some(i64::from(*value)),
        Value::Long(value) => Some(*value),
        Value::Ref(Some(marker)) => ["integer:", "long:", "short:", "byte:"]
            .iter()
            .find_map(|prefix| marker.strip_prefix(prefix))
            .and_then(|digits| digits.parse().ok()),
        _ => None,
    }
}

fn floating(value: &Value) -> Option<f64> {
    match value {
        Value::Float(value) => Some(*value),
        Value::Ref(Some(marker)) => ["double:", "float:"]
            .iter()
            .find_map(|prefix| marker.strip_prefix(prefix))
            .and_then(|digits| digits.parse().ok()),
        _ => None,
    }
}

/// `Comparable.compareTo` for the values whose natural order the JDK defines
/// and the runtime can read.
fn natural_compare(a: &Value, b: &Value) -> Result<i32, NativeError> {
    if matches!(a, Value::Ref(None)) || matches!(b, Value::Ref(None)) {
        return Err(NativeError::UncaughtException {
            class: "java/lang/NullPointerException".into(),
            message: None,
            frame: None,
        });
    }
    if let (Some(a), Some(b)) = (integral(a), integral(b)) {
        return Ok(a.cmp(&b) as i32);
    }
    // `Character.compareTo` is the code-unit difference, not a sign.
    let character = |value: &Value| {
        expect_ref(value)
            .and_then(|marker| marker.strip_prefix("character:"))
            .and_then(|digits| digits.parse::<i32>().ok())
    };
    if let (Some(a), Some(b)) = (character(a), character(b)) {
        return Ok(a - b);
    }
    if let (Some(a), Some(b)) = (floating(a), floating(b)) {
        // `Double.compare`: -0.0 < 0.0 and NaN above everything.
        return Ok(a.total_cmp(&b) as i32);
    }
    let boolean = |value: &Value| match expect_ref(value).map(String::as_str) {
        Some("boolean:true") => Some(true),
        Some("boolean:false") => Some(false),
        _ => None,
    };
    if let (Some(a), Some(b)) = (boolean(a), boolean(b)) {
        return Ok(a.cmp(&b) as i32);
    }
    if let (Value::Ref(Some(a)), Value::Ref(Some(b))) = (a, b) {
        // `String.compareTo`: the first differing UTF-16 unit, else the
        // length difference.
        let (mut left, mut right) = (a.encode_utf16(), b.encode_utf16());
        loop {
            match (left.next(), right.next()) {
                (Some(x), Some(y)) if x != y => return Ok(i32::from(x) - i32::from(y)),
                (Some(_), Some(_)) => continue,
                _ => {
                    return Ok(a.encode_utf16().count() as i32 - b.encode_utf16().count() as i32);
                }
            }
        }
    }
    Err(NativeError::Metadata(format!(
        "natural order of {} and {} is not modelled",
        super::describe_value(a),
        super::describe_value(b)
    )))
}

fn operands(body: &str) -> Result<Vec<Value>, NativeError> {
    super::split_capture_markers(body)?
        .into_iter()
        .map(|marker| super::parse_collection_value(&marker))
        .collect()
}

impl<H: crate::Host> crate::Runtime<'_, H> {
    /// `comparator.compare(a, b)`, for a modelled comparator marker or any
    /// comparator value the callback path can run.
    pub(crate) fn compare_with(
        &mut self,
        comparator: &Value,
        a: &Value,
        b: &Value,
    ) -> Result<i32, NativeError> {
        // `sorted(null)` and `List.sort(null)` mean natural order.
        let Some(marker) = expect_ref(comparator).cloned() else {
            return natural_compare(a, b);
        };
        let Some(body) = marker.strip_prefix(COMPARATOR_PREFIX) else {
            let result = self.apply_callback(comparator, &[a.clone(), b.clone()])?;
            return super::boxed_integer(&result).ok_or_else(|| {
                NativeError::Metadata(format!(
                    "Comparator.compare answered {}",
                    super::describe_value(&result)
                ))
            });
        };
        let (kind, rest) = body.split_once(':').unwrap_or((body, ""));
        match kind {
            "natural" => natural_compare(a, b),
            "reverse" => {
                let inner = operands(rest)?
                    .into_iter()
                    .next()
                    .unwrap_or(Value::Ref(None));
                self.compare_with(&inner, b, a)
            }
            "key" => {
                let key = operands(rest)?
                    .into_iter()
                    .next()
                    .unwrap_or(Value::Ref(None));
                let left = self.apply_callback(&key, std::slice::from_ref(a))?;
                let right = self.apply_callback(&key, std::slice::from_ref(b))?;
                natural_compare(&left, &right)
            }
            "keycmp" => {
                let parts = operands(rest)?;
                let [key, order] = parts.as_slice() else {
                    return Err(NativeError::Metadata("malformed comparator marker".into()));
                };
                let left = self.apply_callback(key, std::slice::from_ref(a))?;
                let right = self.apply_callback(key, std::slice::from_ref(b))?;
                self.compare_with(order, &left, &right)
            }
            "then" => {
                let parts = operands(rest)?;
                let [first, second] = parts.as_slice() else {
                    return Err(NativeError::Metadata("malformed comparator marker".into()));
                };
                match self.compare_with(first, a, b)? {
                    0 => self.compare_with(second, a, b),
                    order => Ok(order),
                }
            }
            "entry-key" | "entry-value" => {
                let (ka, va) = super::map_entry_parts(a)?;
                let (kb, vb) = super::map_entry_parts(b)?;
                if kind == "entry-key" {
                    natural_compare(&ka, &kb)
                } else {
                    natural_compare(&va, &vb)
                }
            }
            _ => Err(NativeError::Metadata(format!(
                "unsupported comparator marker {marker}"
            ))),
        }
    }

    /// Run a `Consumer` on one element: a jar callback, else the host's
    /// `Consumer.accept`.
    fn apply_consumer(&mut self, consumer: &Value, value: &Value) -> Result<(), NativeError> {
        if self
            .invoke_callback(consumer, std::slice::from_ref(value))?
            .is_none()
        {
            self.host.invoke_instance(
                &MethodRef {
                    owner: "java/util/function/Consumer".into(),
                    name: "accept".into(),
                    descriptor: "(Ljava/lang/Object;)V".into(),
                },
                consumer,
                std::slice::from_ref(value),
            )?;
        }
        Ok(())
    }

    /// `Comparator.compare(a, b)` called by jar code on a modelled marker.
    pub(crate) fn invoke_modelled_comparator(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        if r.owner != "java/util/Comparator"
            || r.name != "compare"
            || r.descriptor != "(Ljava/lang/Object;Ljava/lang/Object;)I"
        {
            return Ok(None);
        }
        let Some(receiver) = receiver.filter(|value| {
            expect_ref(value).is_some_and(|marker| marker.starts_with(COMPARATOR_PREFIX))
        }) else {
            return Ok(None);
        };
        let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
            return Ok(None);
        };
        Ok(Some(Value::Int(self.compare_with(receiver, a, b)?)))
    }

    /// `List.sort(comparator)`, `Collections.sort(list)` and
    /// `Collections.sort(list, comparator)` on a list the host holds.
    ///
    /// The host keeps the elements but cannot run a jar comparator, so the
    /// runtime sorts them here and writes them back through the host's own
    /// `clear`/`add`. A list the host does not hold mutably (an immutable
    /// `List.of`) is left to the host.
    pub(crate) fn invoke_list_sort(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let (list, comparator) = match (r.owner.as_str(), r.name.as_str(), r.descriptor.as_str()) {
            ("java/util/Collections", "sort", "(Ljava/util/List;)V") => {
                (args.first().cloned(), Value::Ref(None))
            }
            ("java/util/Collections", "sort", "(Ljava/util/List;Ljava/util/Comparator;)V") => (
                args.first().cloned(),
                args.get(1).cloned().unwrap_or(Value::Ref(None)),
            ),
            (owner, "sort", "(Ljava/util/Comparator;)V") if super::is_list_owner(owner) => (
                receiver.cloned(),
                args.first().cloned().unwrap_or(Value::Ref(None)),
            ),
            _ => return Ok(None),
        };
        let Some(list) = list else {
            return Ok(None);
        };
        let Some(items) = self.host.mutable_collection_values(&list) else {
            return Ok(None);
        };
        let values = items
            .iter()
            .map(|item| super::parse_collection_value(item))
            .collect::<Result<Vec<_>, _>>()?;
        let sorted = self.sort_values(values, &comparator)?;
        let method = |name: &str, descriptor: &str| MethodRef {
            owner: "java/util/List".into(),
            name: name.into(),
            descriptor: descriptor.into(),
        };
        self.host
            .invoke_instance(&method("clear", "()V"), &list, &[])?;
        let add = method("add", "(Ljava/lang/Object;)Z");
        for value in sorted {
            self.host.invoke_instance(&add, &list, &[value])?;
        }
        Ok(Some(Value::Void))
    }

    /// A stable merge sort under `comparator`, as `List.sort` and
    /// `Stream.sorted` are (TimSort keeps equal elements in order).
    pub(crate) fn sort_values(
        &mut self,
        values: Vec<Value>,
        comparator: &Value,
    ) -> Result<Vec<Value>, NativeError> {
        if values.len() <= 1 {
            return Ok(values);
        }
        let mut values = values;
        let right = values.split_off(values.len() / 2);
        let left = self.sort_values(values, comparator)?;
        let right = self.sort_values(right, comparator)?;
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let (mut left, mut right) = (left.into_iter().peekable(), right.into_iter().peekable());
        while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
            if self.compare_with(comparator, a, b)? <= 0 {
                merged.extend(left.next());
            } else {
                merged.extend(right.next());
            }
        }
        merged.extend(left);
        merged.extend(right);
        Ok(merged)
    }

    /// `IntStream`/`LongStream` sources: `range`, `rangeClosed` and `of`.
    pub(crate) fn primitive_stream_source(
        &mut self,
        r: &MethodRef,
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        let long = match r.owner.as_str() {
            "java/util/stream/IntStream" => false,
            "java/util/stream/LongStream" => true,
            _ => return Ok(None),
        };
        let values = match (r.name.as_str(), r.descriptor.as_str()) {
            (
                "range" | "rangeClosed",
                "(II)Ljava/util/stream/IntStream;" | "(JJ)Ljava/util/stream/LongStream;",
            ) => {
                let start = args.first().and_then(integral).unwrap_or_default();
                let end = args.get(1).and_then(integral).unwrap_or_default();
                let end = if r.name == "rangeClosed" {
                    end.saturating_add(1)
                } else {
                    end
                };
                let count = end.saturating_sub(start).max(0);
                if count > MAX_SPLITERATOR_ELEMENTS as i64 {
                    return Err(NativeError::Metadata(format!(
                        "{}.{} spans {count} elements; the bounded stream holds at most {MAX_SPLITERATOR_ELEMENTS}",
                        r.owner, r.name
                    )));
                }
                (start..end)
                    .map(|value| {
                        if long {
                            Value::Long(value)
                        } else {
                            Value::Int(value as i32)
                        }
                    })
                    .collect()
            }
            ("of", "(I)Ljava/util/stream/IntStream;" | "(J)Ljava/util/stream/LongStream;") => {
                args.to_vec()
            }
            ("of", "([I)Ljava/util/stream/IntStream;" | "([J)Ljava/util/stream/LongStream;") => {
                match args.first() {
                    Some(array) => self.array(array)?.clone(),
                    None => return Ok(None),
                }
            }
            ("empty", _) => Vec::new(),
            _ => return Ok(None),
        };
        Ok(Some(self.new_stream(class, method, values)))
    }

    /// The stream stages beyond the interpreter's core set, applied to the
    /// receiver stream's `values`.
    pub(crate) fn more_stream_stages(
        &mut self,
        r: &MethodRef,
        values: &[Value],
        args: &[Value],
        class: &str,
        method: &str,
    ) -> Result<Option<Value>, NativeError> {
        let argument = || args.first().cloned().unwrap_or(Value::Ref(None));
        let returns = r.descriptor.rsplit_once(')').map_or("", |(_, ret)| ret);
        let result = match (r.name.as_str(), r.descriptor.as_str()) {
            ("sorted", "()Ljava/util/stream/Stream;") => {
                let sorted = self.sort_values(values.to_vec(), &Value::Ref(None))?;
                self.new_stream(class, method, sorted)
            }
            ("sorted", "(Ljava/util/Comparator;)Ljava/util/stream/Stream;") => {
                let sorted = self.sort_values(values.to_vec(), &argument())?;
                self.new_stream(class, method, sorted)
            }
            ("sorted", "()Ljava/util/stream/IntStream;" | "()Ljava/util/stream/LongStream;") => {
                let sorted = self.sort_values(values.to_vec(), &Value::Ref(None))?;
                self.new_stream(class, method, sorted)
            }
            // `BinaryOperator.maxBy`/`minBy`: the first of equal extremes wins.
            ("max" | "min", "(Ljava/util/Comparator;)Ljava/util/Optional;") => {
                let comparator = argument();
                let mut best: Option<Value> = None;
                for value in values {
                    best = Some(match best {
                        None => value.clone(),
                        Some(current) => {
                            let order = self.compare_with(&comparator, &current, value)?;
                            let keep = if r.name == "max" {
                                order >= 0
                            } else {
                                order <= 0
                            };
                            if keep { current } else { value.clone() }
                        }
                    });
                }
                best.map_or(Value::Ref(None), super::optional_wrap)
            }
            ("findFirst" | "findAny", "()Ljava/util/Optional;") => values
                .first()
                .cloned()
                .map_or(Value::Ref(None), super::optional_wrap),
            ("reduce", "(Ljava/util/function/BinaryOperator;)Ljava/util/Optional;") => {
                let operator = argument();
                let mut acc: Option<Value> = None;
                for value in values {
                    acc = Some(match acc {
                        None => value.clone(),
                        Some(current) => {
                            self.apply_callback(&operator, &[current, value.clone()])?
                        }
                    });
                }
                acc.map_or(Value::Ref(None), super::optional_wrap)
            }
            (
                "reduce",
                "(Ljava/lang/Object;Ljava/util/function/BinaryOperator;)Ljava/lang/Object;",
            ) => {
                let operator = args.get(1).cloned().unwrap_or(Value::Ref(None));
                let mut acc = argument();
                for value in values {
                    acc = self.apply_callback(&operator, &[acc, value.clone()])?;
                }
                acc
            }
            (
                "mapToObj" | "mapToInt" | "mapToLong" | "mapToDouble" | "boxed" | "asLongStream",
                _,
            ) if returns.starts_with("Ljava/util/stream/") => {
                let mapped = if r.name == "boxed" || r.name == "asLongStream" {
                    values.to_vec()
                } else {
                    let function = argument();
                    let mut mapped = Vec::with_capacity(values.len());
                    for value in values {
                        mapped.push(self.apply_callback(&function, std::slice::from_ref(value))?);
                    }
                    mapped
                };
                self.new_stream(class, method, mapped)
            }
            // The stream's elements through the host's iterator table, the
            // same iterator a `List.iterator()` over them answers.
            ("iterator", "()Ljava/util/Iterator;") => self.host.invoke_instance(
                &MethodRef {
                    owner: "java/util/List".into(),
                    name: "iterator".into(),
                    descriptor: "()Ljava/util/Iterator;".into(),
                },
                &super::collection_value("list", values),
                &[],
            )?,
            ("forEachOrdered", "(Ljava/util/function/Consumer;)V") => {
                let consumer = argument();
                for value in values {
                    self.apply_consumer(&consumer, value)?;
                }
                Value::Void
            }
            ("sum", "()I") => Value::Int(
                values
                    .iter()
                    .filter_map(integral)
                    .fold(0i32, |sum, value| sum.wrapping_add(value as i32)),
            ),
            ("sum", "()J") => Value::Long(
                values
                    .iter()
                    .filter_map(integral)
                    .fold(0i64, i64::wrapping_add),
            ),
            ("sum", "()D") => Value::Float(values.iter().filter_map(floating).sum()),
            _ => return Ok(None),
        };
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Value {
        Value::Ref(Some(value.into()))
    }

    #[test]
    fn natural_order_matches_the_jdk_compare_to() {
        assert_eq!(
            natural_compare(&text("integer:3"), &Value::Int(7)).unwrap(),
            -1
        );
        assert_eq!(
            natural_compare(&Value::Long(9), &text("long:2")).unwrap(),
            1
        );
        // "apple".compareTo("apricot") == 'p' - 'r'
        assert_eq!(
            natural_compare(&text("apple"), &text("apricot")).unwrap(),
            -2
        );
        // "ab".compareTo("abcd") == 2 - 4
        assert_eq!(natural_compare(&text("ab"), &text("abcd")).unwrap(), -2);
        assert_eq!(
            natural_compare(&text("boolean:false"), &text("boolean:true")).unwrap(),
            -1
        );
        assert!(matches!(
            natural_compare(&Value::Ref(None), &Value::Int(1)),
            Err(NativeError::UncaughtException { class, .. })
                if class == "java/lang/NullPointerException"
        ));
    }

    #[test]
    fn comparator_combinators_nest_their_operands() {
        let reversed = comparator_instance(
            &MethodRef {
                owner: "java/util/Comparator".into(),
                name: "reversed".into(),
                descriptor: "()Ljava/util/Comparator;".into(),
            },
            &text("@indy:demo/A#x:1:demo/A#cmp(II)I:"),
            &[],
        )
        .unwrap();
        let marker = expect_ref(&reversed).unwrap();
        let inner = operands(marker.strip_prefix("@cmp:reverse:").unwrap()).unwrap();
        assert_eq!(inner, [text("@indy:demo/A#x:1:demo/A#cmp(II)I:")]);
    }
}

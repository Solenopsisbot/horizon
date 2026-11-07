//! MixinExtras injectors (`com.llamalad7.mixinextras`) in plan discovery and
//! the game-hook tier.
//!
//! MixinExtras adds injectors that Mixin's own discovery never names, so a
//! jar's `@ModifyReturnValue` or `@WrapOperation` used to be invisible: not a
//! plan, not a skip, only (for an instance handler) a generic
//! `@Mixin-member` row. [`NativeJar::mixin_extras_plans`] reads them all,
//! under any package a jar relocates MixinExtras to (the annotation's
//! descriptor ends in `/mixinextras/injector/...`):
//!
//! | Injector | Where it acts | Tier |
//! |---|---|---|
//! | `@ModifyReturnValue` | each `RETURN` of the target | binds at a value site |
//! | `@WrapMethod` | the whole target, around its body | discovered |
//! | `@ModifyExpressionValue` | the value of one expression in the body | discovered |
//! | `@ModifyReceiver` | the receiver of one call in the body | discovered |
//! | `@WrapOperation` | one call, field access or `instanceof` in the body | discovered |
//! | `@WrapWithCondition` | one `void` call in the body | discovered |
//!
//! **What binds.** A `@ModifyReturnValue` handler takes the value the target
//! is about to return (and, optionally, the target's arguments) and answers
//! the value to return instead. At a game-hook site that fires at the
//! target's `RETURN` with an override contract (`java_destroy_speed`), that
//! is exactly what the host offers: it hands the method's result in and uses
//! the override. Handlers chain in apply order, each receiving the previous
//! one's answer, as MixinExtras chains them; `@Inject` callbacks at the same
//! point see the result as it stands when they run.
//!
//! **What does not, and why.** `@WrapMethod` hands its handler an `Operation`
//! whose `call` runs the original method; a hook point fires inside Horizon's
//! own code, which the interpreter cannot call back into mid-handler, so the
//! wrapper cannot be honoured (it may call the original twice, or not at
//! all). The other four act on one instruction inside the body, and no hook
//! point brackets a single instruction. They stay skipped with a sentence
//! that names the injector.
//!
//! `@Local` and the other sugar annotations sit on handler parameters; the
//! class reader keeps no parameter annotations, so they are not counted.

use super::*;

/// The injector a MixinExtras annotation names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MixinExtrasKind {
    ModifyReturnValue,
    WrapMethod,
    ModifyExpressionValue,
    ModifyReceiver,
    WrapOperation,
    WrapWithCondition,
}

impl MixinExtrasKind {
    /// The injector from an annotation descriptor, under any relocation.
    pub fn from_descriptor(descriptor: &str) -> Option<Self> {
        let (_, rest) = descriptor.split_once("/mixinextras/injector/")?;
        Some(match rest {
            "ModifyReturnValue;" => Self::ModifyReturnValue,
            "wrapmethod/WrapMethod;" => Self::WrapMethod,
            "ModifyExpressionValue;" => Self::ModifyExpressionValue,
            "ModifyReceiver;" => Self::ModifyReceiver,
            "wrapoperation/WrapOperation;" => Self::WrapOperation,
            "WrapWithCondition;" | "v2/WrapWithCondition;" => Self::WrapWithCondition,
            _ => return None,
        })
    }

    /// The annotation as a report spells it.
    pub fn label(self) -> &'static str {
        match self {
            Self::ModifyReturnValue => "@ModifyReturnValue",
            Self::WrapMethod => "@WrapMethod",
            Self::ModifyExpressionValue => "@ModifyExpressionValue",
            Self::ModifyReceiver => "@ModifyReceiver",
            Self::WrapOperation => "@WrapOperation",
            Self::WrapWithCondition => "@WrapWithCondition",
        }
    }

    /// Whether the injector acts on one instruction inside the body.
    pub fn mid_method(self) -> bool {
        !matches!(self, Self::ModifyReturnValue | Self::WrapMethod)
    }
}

/// One MixinExtras injector a `@Mixin` class declares, for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixinExtrasPlan {
    pub kind: MixinExtrasKind,
    /// The target method; an empty descriptor when the selector named only a
    /// name, as Mixin's `method = "tick"`.
    pub target: MethodRef,
    pub callback: MethodRef,
    /// The `@At` anchor (`RETURN`, `INVOKE`...); empty for `@WrapMethod`,
    /// which has none.
    pub at: String,
    /// The `@At.target` selector, as written.
    pub site: Option<String>,
    /// `@At.ordinal`, when given.
    pub ordinal: Option<i32>,
    /// Whether the injector declares a `slice`.
    pub sliced: bool,
}

/// How a bound game-hook callback is called and what its answer does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GameHookShape {
    /// An `@Inject` callback: the target's arguments and a `CallbackInfo`.
    #[default]
    Inject,
    /// A `@ModifyReturnValue` handler: the value about to be returned (then,
    /// optionally, the target's arguments); its answer replaces that value.
    ModifyReturnValue,
}

impl NativeJar {
    /// Every MixinExtras injector the jar's `@Mixin` classes declare, one per
    /// (target class, method selector, `@At`), sorted by callback then target.
    pub fn mixin_extras_plans(&self) -> Vec<MixinExtrasPlan> {
        let mut plans = Vec::new();
        for class in self.classes.values() {
            let Some(mixin) = class
                .annotations
                .iter()
                .find(|annotation| annotation.descriptor == MIXIN_ANNOTATION_DESCRIPTOR)
            else {
                continue;
            };
            let targets = mixin_target_owners(mixin);
            for method in &class.methods {
                for annotation in &method.annotations {
                    let Some(kind) = MixinExtrasKind::from_descriptor(&annotation.descriptor)
                    else {
                        continue;
                    };
                    let specs = annotation
                        .values
                        .get("method")
                        .map(annotation_strings)
                        .unwrap_or_default();
                    // `@At` is one annotation (`@ModifyReturnValue`) or an
                    // array (`@WrapOperation`'s); `@WrapMethod` has none.
                    let anchors = annotation
                        .values
                        .get("at")
                        .map(annotation_anchor_selectors)
                        .unwrap_or_default();
                    let anchors: Vec<(String, Option<String>, Option<i32>)> = if anchors.is_empty()
                    {
                        vec![(String::new(), None, None)]
                    } else {
                        anchors
                            .into_iter()
                            .map(|at| (at.anchor, at.target, at.ordinal))
                            .collect()
                    };
                    let sliced = annotation.values.contains_key("slice");
                    for owner in &targets {
                        for spec in &specs {
                            let Some((name, descriptor)) = split_injection_method(spec) else {
                                continue;
                            };
                            for (at, site, ordinal) in &anchors {
                                plans.push(MixinExtrasPlan {
                                    kind,
                                    target: MethodRef {
                                        owner: owner.clone(),
                                        name: name.to_owned(),
                                        descriptor: descriptor.to_owned(),
                                    },
                                    callback: MethodRef {
                                        owner: class.name.clone(),
                                        name: method.name.clone(),
                                        descriptor: method.descriptor.clone(),
                                    },
                                    at: at.clone(),
                                    site: site.clone(),
                                    ordinal: *ordinal,
                                    sliced,
                                });
                            }
                        }
                    }
                }
            }
        }
        plans.sort_by(|a, b| {
            (&a.callback.owner, &a.callback.name, &a.callback.descriptor)
                .cmp(&(&b.callback.owner, &b.callback.name, &b.callback.descriptor))
                .then_with(|| {
                    (&a.target.owner, &a.target.name, &a.at).cmp(&(
                        &b.target.owner,
                        &b.target.name,
                        &b.at,
                    ))
                })
        });
        plans.dedup();
        plans
    }

    /// One MixinExtras plan in the game-hook tier: a hook plan when it binds,
    /// `Ok(None)` when the plain weave decides (a target in the jar), else
    /// why it stays out.
    pub(crate) fn mixin_extras_game_hook_plan(
        &self,
        plan: &MixinExtrasPlan,
        side: MixinSide,
        configs: &[MixinConfig],
        config_sides: &BTreeMap<String, MixinSide>,
        plugins: &ConfigPlugins,
    ) -> Result<GameHookPlan, MixinSkipReason> {
        let anchor = if plan.at.is_empty() {
            "HEAD"
        } else {
            plan.at.as_str()
        };
        let site = game_hook_site_at(&plan.target, anchor);
        if plan.kind != MixinExtrasKind::ModifyReturnValue {
            return Err(match site {
                Some(site) => MixinSkipReason::GameHookMismatch(bounded_weave_reason(&format!(
                    "{} {}; hook point {} cannot honour it",
                    plan.kind.label(),
                    if plan.kind.mid_method() {
                        "acts on one instruction inside the body, which no hook point brackets"
                    } else {
                        "hands its handler the original method to call, which a hook point cannot"
                    },
                    site.hook
                ))),
                None if self.classes.contains_key(&plan.target.owner) => {
                    MixinSkipReason::UnsupportedFamily
                }
                None => MixinSkipReason::TargetOutsideJar,
            });
        }
        let Some(site) = site else {
            return Err(if self.classes.contains_key(&plan.target.owner) {
                MixinSkipReason::UnsupportedFamily
            } else {
                MixinSkipReason::TargetOutsideJar
            });
        };
        self.game_hook_config_gate(
            configs,
            config_sides,
            plugins,
            &plan.callback.owner,
            &plan.target.owner,
            side,
        )?;
        if plan.ordinal.is_some() || plan.sliced {
            return Err(MixinSkipReason::SelectorModifier);
        }
        let mismatch = |text: String| {
            Err(MixinSkipReason::GameHookMismatch(bounded_weave_reason(
                &text,
            )))
        };
        if !site.returns_value() || site.fires_at_head() || !site.anchors.contains(&anchor) {
            return mismatch(format!(
                "@ModifyReturnValue needs a value returned at RETURN; hook point {} does not fire there",
                site.hook
            ));
        }
        // An override contract is what lets the host use the new value; the
        // handler always replaces it, so it is cancellable in `@Inject` terms.
        game_hook_site_fits(site, anchor, true, side)?;
        modify_return_callback(&self.classes, &plan.callback, site)?;
        Ok(GameHookPlan {
            hook: site.hook,
            version: site.version,
            anchor: anchor.to_owned(),
            plan: MixinInjectionPlan {
                target: MethodRef {
                    descriptor: site.target_descriptor.to_owned(),
                    ..plan.target.clone()
                },
                callback: plan.callback.clone(),
                at: anchor.to_owned(),
                cancellable: true,
                invoke_target: None,
                ordinal: None,
                slice: None,
                shift: None,
            },
            shape: GameHookShape::ModifyReturnValue,
        })
    }
}

/// Check a `@ModifyReturnValue` handler against `site`: it returns the
/// target's return type and takes that value, optionally followed by the
/// target's arguments. Answers whether it takes them.
pub(crate) fn modify_return_callback(
    classes: &BTreeMap<String, ClassFile>,
    callback: &MethodRef,
    site: &GameHookSite,
) -> Result<bool, MixinSkipReason> {
    let mismatch = |text: String| MixinSkipReason::GameHookMismatch(bounded_weave_reason(&text));
    let method = classes
        .get(&callback.owner)
        .and_then(|class| class.method(&callback.name, &callback.descriptor))
        .ok_or_else(|| {
            mismatch(format!(
                "@ModifyReturnValue handler {}#{}{} is not in this jar",
                callback.owner, callback.name, callback.descriptor
            ))
        })?;
    let returns = return_desc(site.target_descriptor);
    let arguments = descriptor_arguments(&method.descriptor);
    let target_arguments = descriptor_arguments(site.target_descriptor);
    let takes_arguments =
        arguments.len() == 1 + target_arguments.len() && arguments[1..] == target_arguments[..];
    if return_desc(&method.descriptor) != returns
        || arguments.first().map(String::as_str) != Some(returns)
        || !(arguments.len() == 1 || takes_arguments)
    {
        return Err(mismatch(format!(
            "@ModifyReturnValue handler {}{} does not take and return {} (then optionally the arguments of {}#{}{})",
            callback.name,
            callback.descriptor,
            returns,
            site.target_owner,
            site.target_name,
            site.target_descriptor
        )));
    }
    Ok(takes_arguments)
}

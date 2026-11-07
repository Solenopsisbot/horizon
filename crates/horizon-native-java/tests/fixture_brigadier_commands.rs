//! A source-built Fabric jar registers Brigadier commands the way real server
//! mods do, and its `executes` callbacks run with the arguments the game
//! parsed.
//!
//! The Brigadier, game and loader classes are compiled as stubs so `javac`
//! accepts the mod, but only the mod's own classes go into the jar: the native
//! host answers the API classes, exactly as it does for a real jar.

use horizon_native_java::{
    ExecutionBudget, NativeJar, StdHost,
    brigadier::{
        ArgumentKind, CommandArgument, CommandInvocation, CommandMessage, CommandOutcome,
        CommandSource, CommandText, MessageKind, NodeKind, Requirement,
    },
    events::{EventContext, EventFacade, EventFiring, FacadePlayer, GameEvent},
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};
use zip::write::SimpleFileOptions;

const STUBS: &[(&str, &str)] = &[
    (
        "net/fabricmc/fabric/api/event/Event.java",
        "package net.fabricmc.fabric.api.event;\n\
         public abstract class Event<T> { public void register(T listener) {} }",
    ),
    (
        "net/fabricmc/fabric/api/command/v2/CommandRegistrationCallback.java",
        "package net.fabricmc.fabric.api.command.v2;\n\
         public interface CommandRegistrationCallback {\n\
           net.fabricmc.fabric.api.event.Event<CommandRegistrationCallback> EVENT = null;\n\
           void register(com.mojang.brigadier.CommandDispatcher<net.minecraft.commands.CommandSourceStack> d,\n\
             net.minecraft.commands.CommandBuildContext c, net.minecraft.commands.Commands.CommandSelection s); }",
    ),
    (
        "com/mojang/brigadier/CommandDispatcher.java",
        "package com.mojang.brigadier;\n\
         public class CommandDispatcher<S> {\n\
           public com.mojang.brigadier.tree.LiteralCommandNode<S> register(\n\
             com.mojang.brigadier.builder.LiteralArgumentBuilder<S> b) { return null; } }",
    ),
    (
        "com/mojang/brigadier/Command.java",
        "package com.mojang.brigadier;\n\
         public interface Command<S> { int run(com.mojang.brigadier.context.CommandContext<S> c)\n\
           throws com.mojang.brigadier.exceptions.CommandSyntaxException; }",
    ),
    (
        "com/mojang/brigadier/exceptions/CommandSyntaxException.java",
        "package com.mojang.brigadier.exceptions;\n\
         public class CommandSyntaxException extends Exception {}",
    ),
    (
        "com/mojang/brigadier/context/CommandContext.java",
        "package com.mojang.brigadier.context;\n\
         public class CommandContext<S> { public S getSource() { return null; } }",
    ),
    (
        "com/mojang/brigadier/tree/LiteralCommandNode.java",
        "package com.mojang.brigadier.tree;\n\
         public class LiteralCommandNode<S> {}",
    ),
    (
        "com/mojang/brigadier/builder/ArgumentBuilder.java",
        "package com.mojang.brigadier.builder;\n\
         public abstract class ArgumentBuilder<S, T extends ArgumentBuilder<S, T>> {\n\
           public T then(ArgumentBuilder<S, ?> a) { return null; }\n\
           public T executes(com.mojang.brigadier.Command<S> c) { return null; }\n\
           public T requires(java.util.function.Predicate<S> p) { return null; } }",
    ),
    (
        "com/mojang/brigadier/builder/LiteralArgumentBuilder.java",
        "package com.mojang.brigadier.builder;\n\
         public class LiteralArgumentBuilder<S> extends ArgumentBuilder<S, LiteralArgumentBuilder<S>> {}",
    ),
    (
        "com/mojang/brigadier/builder/RequiredArgumentBuilder.java",
        "package com.mojang.brigadier.builder;\n\
         public class RequiredArgumentBuilder<S, T> extends ArgumentBuilder<S, RequiredArgumentBuilder<S, T>> {}",
    ),
    (
        "com/mojang/brigadier/arguments/ArgumentType.java",
        "package com.mojang.brigadier.arguments;\n\
         public interface ArgumentType<T> {}",
    ),
    (
        "com/mojang/brigadier/arguments/IntegerArgumentType.java",
        "package com.mojang.brigadier.arguments;\n\
         public class IntegerArgumentType implements ArgumentType<Integer> {\n\
           public static IntegerArgumentType integer(int min, int max) { return null; }\n\
           public static int getInteger(com.mojang.brigadier.context.CommandContext<?> c, String n) { return 0; } }",
    ),
    (
        "com/mojang/brigadier/arguments/StringArgumentType.java",
        "package com.mojang.brigadier.arguments;\n\
         public class StringArgumentType implements ArgumentType<String> {\n\
           public static StringArgumentType word() { return null; }\n\
           public static String getString(com.mojang.brigadier.context.CommandContext<?> c, String n) { return null; } }",
    ),
    (
        "net/minecraft/commands/CommandBuildContext.java",
        "package net.minecraft.commands;\npublic interface CommandBuildContext {}",
    ),
    (
        "net/minecraft/commands/Commands.java",
        "package net.minecraft.commands;\n\
         import com.mojang.brigadier.builder.*;\n\
         import net.minecraft.server.permissions.*;\n\
         public class Commands {\n\
           public static final PermissionCheck LEVEL_GAMEMASTERS = null;\n\
           public enum CommandSelection { ALL, DEDICATED, INTEGRATED }\n\
           public static LiteralArgumentBuilder<CommandSourceStack> literal(String s) { return null; }\n\
           public static <T> RequiredArgumentBuilder<CommandSourceStack, T> argument(String s,\n\
             com.mojang.brigadier.arguments.ArgumentType<T> t) { return null; }\n\
           public static <T> PermissionProviderCheck<T> hasPermission(PermissionCheck c) { return null; } }",
    ),
    (
        "net/minecraft/commands/CommandSourceStack.java",
        "package net.minecraft.commands;\n\
         public class CommandSourceStack {\n\
           public void sendSuccess(java.util.function.Supplier<net.minecraft.network.chat.Component> s, boolean b) {}\n\
           public void sendSystemMessage(net.minecraft.network.chat.Component c) {}\n\
           public net.minecraft.server.permissions.PermissionSet permissions() { return null; }\n\
           public net.minecraft.server.level.ServerPlayer getPlayerOrException()\n\
             throws com.mojang.brigadier.exceptions.CommandSyntaxException { return null; }\n\
           public Object registryAccess() { return null; } }",
    ),
    (
        "net/minecraft/server/permissions/PermissionCheck.java",
        "package net.minecraft.server.permissions;\npublic interface PermissionCheck {}",
    ),
    (
        "net/minecraft/server/permissions/PermissionProviderCheck.java",
        "package net.minecraft.server.permissions;\n\
         public final class PermissionProviderCheck<T> implements java.util.function.Predicate<T> {\n\
           public boolean test(T t) { return false; } }",
    ),
    (
        "net/minecraft/server/permissions/Permission.java",
        "package net.minecraft.server.permissions;\npublic interface Permission {}",
    ),
    (
        "net/minecraft/server/permissions/Permissions.java",
        "package net.minecraft.server.permissions;\n\
         public class Permissions { public static final Permission COMMANDS_MODERATOR = null; }",
    ),
    (
        "net/minecraft/server/permissions/PermissionSet.java",
        "package net.minecraft.server.permissions;\n\
         public interface PermissionSet { boolean hasPermission(Permission p); }",
    ),
    (
        "net/minecraft/network/chat/Component.java",
        "package net.minecraft.network.chat;\n\
         public interface Component { static MutableComponent literal(String s) { return null; }\n\
           static MutableComponent translatable(String k, Object... args) { return null; } }",
    ),
    (
        "net/minecraft/network/chat/MutableComponent.java",
        "package net.minecraft.network.chat;\n\
         public class MutableComponent implements Component {\n\
           public MutableComponent withStyle(net.minecraft.ChatFormatting f) { return this; }\n\
           public MutableComponent append(Component c) { return this; }\n\
           public MutableComponent append(String s) { return this; } }",
    ),
    (
        "net/minecraft/ChatFormatting.java",
        "package net.minecraft;\npublic enum ChatFormatting { AQUA, GOLD, BOLD }",
    ),
    (
        "net/minecraft/server/level/ServerPlayer.java",
        "package net.minecraft.server.level;\n\
         public class ServerPlayer { public String getScoreboardName() { return null; } }",
    ),
];

/// `/greet <count> <name>` (operators, a level check), `/whoami` (a `requires`
/// lambda at moderator level) and `/boom`, whose body reaches past the façade.
const MOD: &str = r#"
package demo;
import com.mojang.brigadier.arguments.IntegerArgumentType;
import com.mojang.brigadier.arguments.StringArgumentType;
import net.fabricmc.fabric.api.command.v2.CommandRegistrationCallback;
import net.minecraft.commands.Commands;
import net.minecraft.network.chat.Component;
import net.minecraft.server.permissions.Permissions;
public final class CmdMod {
  public static int greeted;
  public void onInitialize() {
    CommandRegistrationCallback.EVENT.register((dispatcher, context, selection) -> {
      dispatcher.register(Commands.literal("greet")
        .requires(Commands.hasPermission(Commands.LEVEL_GAMEMASTERS))
        .then(Commands.argument("count", IntegerArgumentType.integer(1, 64))
          .then(Commands.argument("name", StringArgumentType.word())
            .executes(ctx -> {
              int count = IntegerArgumentType.getInteger(ctx, "count");
              String name = StringArgumentType.getString(ctx, "name");
              greeted += count;
              ctx.getSource().sendSuccess(() -> Component.literal("Hello " + name + " x" + count), true);
              return count * 10;
            }))));
      dispatcher.register(Commands.literal("whoami")
        .requires(source -> source.permissions().hasPermission(Permissions.COMMANDS_MODERATOR))
        .executes(ctx -> {
          String name = ctx.getSource().getPlayerOrException().getScoreboardName();
          // Styled, with a translated sibling that takes arguments.
          ctx.getSource().sendSystemMessage(Component.literal("You are ")
            .append(Component.literal(name).withStyle(net.minecraft.ChatFormatting.AQUA))
            .append(", greeted " + greeted)
            .append(Component.translatable("demo.times", greeted, name).withStyle(net.minecraft.ChatFormatting.GOLD)));
          return 1;
        }));
      dispatcher.register(Commands.literal("boom").executes(ctx -> {
        ctx.getSource().registryAccess();
        return 1;
      }));
    });
  }
}
"#;

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-brigadier-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// The mod compiled against the stubs, its own classes packed with a
/// `fabric.mod.json`. `None` without a Java toolchain.
fn build_jar(root: &Path) -> Option<PathBuf> {
    let src = root.join("src");
    let classes = root.join("classes");
    let mut paths = Vec::new();
    for (path, text) in STUBS.iter().chain([("demo/CmdMod.java", MOD)].iter()) {
        let file = src.join(path);
        fs::create_dir_all(file.parent()?).ok()?;
        fs::write(&file, text).ok()?;
        paths.push(file);
    }
    fs::create_dir_all(&classes).ok()?;
    let javac = std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/javac"))
        .unwrap_or_else(|| "javac".into());
    let status = Command::new(javac)
        .args(["--release", "17", "-d"])
        .arg(&classes)
        .args(&paths)
        .status()
        .ok()?;
    assert!(status.success(), "javac failed");
    let jar_path = root.join("mod.jar");
    let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).ok()?);
    zip.start_file("fabric.mod.json", SimpleFileOptions::default())
        .ok()?;
    zip.write_all(br#"{"id":"cmd_mod","version":"1.0","entrypoints":{"main":["demo.CmdMod"]}}"#)
        .ok()?;
    for entry in fs::read_dir(classes.join("demo")).ok()? {
        let path = entry.ok()?.path();
        let name = format!("demo/{}", path.file_name()?.to_string_lossy());
        zip.start_file(name, SimpleFileOptions::default()).ok()?;
        zip.write_all(&fs::read(&path).ok()?).ok()?;
    }
    zip.finish().ok()?;
    Some(jar_path)
}

fn budget() -> ExecutionBudget {
    ExecutionBudget::within(Duration::from_secs(5))
}

const ANA: u128 = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef;

fn facade() -> Arc<EventFacade> {
    Arc::new(EventFacade {
        dedicated: true,
        players: vec![FacadePlayer {
            uuid: ANA,
            name: "Ana".into(),
            level: "minecraft:overworld".into(),
            entity_id: 7,
        }],
        ..EventFacade::default()
    })
}

fn source(level: u8, player: Option<u128>) -> CommandSource {
    CommandSource {
        name: if player.is_some() { "Ana" } else { "Server" }.into(),
        level,
        player,
        dimension: "minecraft:overworld".into(),
    }
}

#[test]
fn a_registration_callback_builds_a_typed_tree_and_its_commands_run() {
    let root = scratch("fabric");
    let Some(jar_path) = build_jar(&root) else {
        eprintln!("skipped: no javac");
        return;
    };
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();

    // The registration is a listener of the command-tree build.
    let listeners: Vec<_> = host
        .game_listeners(&jar, true)
        .listeners
        .into_iter()
        .filter(|l| l.event == GameEvent::RegisterCommands)
        .collect();
    assert_eq!(listeners.len(), 1);
    assert_eq!(listeners[0].source, "CommandRegistrationCallback.EVENT");
    // Nothing is in the tree before the server builds it.
    assert!(host.command_tree().nodes.is_empty());
    let mut firing = EventFiring::new(
        GameEvent::RegisterCommands,
        facade(),
        EventContext::default(),
    );
    jar.run_game_listener(&listeners[0], &mut firing, &mut host, &budget())
        .unwrap();

    let tree = host.command_tree();
    let roots: Vec<(&str, Option<&Requirement>)> = tree
        .roots()
        .map(|(_, node)| (node.kind.name(), node.requirement.as_ref()))
        .collect();
    assert_eq!(roots[0], ("greet", Some(&Requirement::Level(2))));
    assert_eq!(roots[1].0, "whoami");
    assert!(matches!(roots[1].1, Some(Requirement::Predicate(_))));
    assert_eq!(roots[2], ("boom", None));
    let (_, greet) = tree.roots().next().unwrap();
    assert!(greet.command.is_none());
    let count = &tree.nodes[greet.children[0]];
    assert_eq!(
        count.kind,
        NodeKind::Argument {
            name: "count".into(),
            kind: ArgumentKind::Integer { min: 1, max: 64 }
        }
    );
    assert!(count.command.is_none());
    let name = &tree.nodes[count.children[0]];
    assert_eq!(
        name.kind,
        NodeKind::Argument {
            name: "name".into(),
            kind: ArgumentKind::Word
        }
    );
    let greet_command = name.command.clone().expect("executes");

    // The lambda requirement is classified by running it: moderators.
    let (_, whoami) = tree.roots().nth(1).unwrap();
    let requirement = whoami.requirement.clone().unwrap();
    assert_eq!(
        jar.requirement_level(&requirement, true, &mut host, &budget())
            .unwrap(),
        1
    );
    assert!(
        !jar.check_requirement(
            &requirement,
            &source(0, Some(ANA)),
            facade(),
            &mut host,
            &budget()
        )
        .unwrap()
    );

    // `/greet 3 Bob`: the callback reads the parsed int and string, and its
    // `sendSuccess` supplier makes the feedback line.
    let run = jar.run_command(
        &greet_command,
        &CommandInvocation {
            input: "greet 3 Bob".into(),
            source: source(2, Some(ANA)),
            arguments: vec![
                ("count".into(), CommandArgument::Int(3)),
                ("name".into(), CommandArgument::Str("Bob".into())),
            ],
        },
        facade(),
        &mut host,
        &budget(),
    );
    assert_eq!(run.outcome, CommandOutcome::Returned(30));
    assert_eq!(
        run.messages,
        [CommandMessage {
            kind: MessageKind::Success { broadcast: true },
            text: CommandText {
                text: "Hello Bob x3".into(),
                translate: false,
                component: None,
            },
        }]
    );

    // `/whoami` from a player sees the player's name and the static the
    // previous command wrote; from the console it is vanilla's syntax error.
    let whoami_command = whoami.command.clone().unwrap();
    let as_player = jar.run_command(
        &whoami_command,
        &CommandInvocation {
            input: "whoami".into(),
            source: source(1, Some(ANA)),
            arguments: Vec::new(),
        },
        facade(),
        &mut host,
        &budget(),
    );
    assert_eq!(as_player.outcome, CommandOutcome::Returned(1));
    assert_eq!(as_player.messages[0].kind, MessageKind::System);
    // The plain text reads the key as is; the component keeps the colours,
    // the sibling order and the translation's arguments as vanilla's JSON.
    assert_eq!(
        as_player.messages[0].text.text,
        "You are Ana, greeted 3demo.times"
    );
    let component: serde_json::Value =
        serde_json::from_str(as_player.messages[0].text.component.as_deref().unwrap()).unwrap();
    assert_eq!(
        component,
        serde_json::json!({
            "text": "You are ",
            "extra": [
                {"text": "Ana", "color": "aqua"},
                {"text": ", greeted 3"},
                {"translate": "demo.times", "with": [3, "Ana"], "color": "gold"},
            ],
        })
    );
    let as_console = jar.run_command(
        &whoami_command,
        &CommandInvocation {
            input: "whoami".into(),
            source: source(4, None),
            arguments: Vec::new(),
        },
        facade(),
        &mut host,
        &budget(),
    );
    assert_eq!(
        as_console.outcome,
        CommandOutcome::Syntax(CommandText {
            text: "permissions.requires.player".into(),
            translate: true,
            component: None,
        })
    );

    // `/boom` reaches a member the façade does not model: refused, by name.
    let (_, boom) = tree.roots().nth(2).unwrap();
    let refused = jar.run_command(
        boom.command.as_ref().unwrap(),
        &CommandInvocation {
            input: "boom".into(),
            source: source(4, None),
            arguments: Vec::new(),
        },
        facade(),
        &mut host,
        &budget(),
    );
    match refused.outcome {
        CommandOutcome::Refused(reason) => assert!(reason.contains("registryAccess"), "{reason}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let _ = fs::remove_dir_all(&root);
}

//! The registry ids of every `Registries`/`BuiltInRegistries` field in
//! vanilla 26.3.
//!
//! Read from the 26.3 server jar's `Registries` (each `createRegistryKey`
//! string) and `BuiltInRegistries` (the `Registries` key each registry field is
//! built from) static initialisers. Field names differ between the two
//! classes in a few places (`BuiltInRegistries.TRIGGER_TYPES` is
//! `minecraft:trigger_type`), which is why both are listed. These are names,
//! not behaviour: a registry read through either class answers a
//! `registry/<id>` marker instead of null, so a library that indexes its
//! wrappers by `registry.key().identifier()` sees one id per registry.

/// `net.minecraft.core.registries.Registries` field → registry id.
const REGISTRIES_KEYS: &[(&str, &str)] = &[
    ("ACTIVITY", "minecraft:activity"),
    ("ADVANCEMENT", "minecraft:advancement"),
    ("ATTRIBUTE", "minecraft:attribute"),
    ("ATTRIBUTE_TYPE", "minecraft:attribute_type"),
    ("BANNER_PATTERN", "minecraft:banner_pattern"),
    ("BIOME", "minecraft:worldgen/biome"),
    ("BIOME_SOURCE", "minecraft:worldgen/biome_source"),
    ("BLOCK", "minecraft:block"),
    ("BLOCK_ENTITY_TYPE", "minecraft:block_entity_type"),
    ("BLOCK_PREDICATE_TYPE", "minecraft:block_predicate_type"),
    (
        "BLOCK_STATE_PROVIDER",
        "minecraft:worldgen/block_state_provider",
    ),
    (
        "BLOCK_STATE_PROVIDER_TYPE",
        "minecraft:worldgen/block_state_provider_type",
    ),
    ("BLOCK_TRANSFORMER", "minecraft:block_transformer"),
    ("CARVER", "minecraft:worldgen/carver"),
    ("CARVER_TYPE", "minecraft:worldgen/carver_type"),
    ("CAT_SOUND_VARIANT", "minecraft:cat_sound_variant"),
    ("CAT_VARIANT", "minecraft:cat_variant"),
    ("CHAT_TYPE", "minecraft:chat_type"),
    ("CHICKEN_SOUND_VARIANT", "minecraft:chicken_sound_variant"),
    ("CHICKEN_VARIANT", "minecraft:chicken_variant"),
    ("CHUNK_GENERATOR", "minecraft:worldgen/chunk_generator"),
    ("CHUNK_STATUS", "minecraft:chunk_status"),
    ("COMMAND_ARGUMENT_TYPE", "minecraft:command_argument_type"),
    ("CONSUME_EFFECT_TYPE", "minecraft:consume_effect_type"),
    ("CONTEXT_FLOAT_PROVIDER", "minecraft:context_float_provider"),
    (
        "CONTEXT_FLOAT_PROVIDER_TYPE",
        "minecraft:context_float_provider_type",
    ),
    ("CONTEXT_INT_PROVIDER", "minecraft:context_int_provider"),
    (
        "CONTEXT_INT_PROVIDER_TYPE",
        "minecraft:context_int_provider_type",
    ),
    ("CONTEXT_KEY_SET", "minecraft:context_key_set"),
    ("COW_SOUND_VARIANT", "minecraft:cow_sound_variant"),
    ("COW_VARIANT", "minecraft:cow_variant"),
    ("CREATIVE_MODE_TAB", "minecraft:creative_mode_tab"),
    ("CUSTOM_STAT", "minecraft:custom_stat"),
    ("DAMAGE_TYPE", "minecraft:damage_type"),
    (
        "DATA_COMPONENT_PREDICATE_TYPE",
        "minecraft:data_component_predicate_type",
    ),
    ("DATA_COMPONENT_TYPE", "minecraft:data_component_type"),
    ("DEBUG_SUBSCRIPTION", "minecraft:debug_subscription"),
    ("DECORATED_POT_PATTERN", "minecraft:decorated_pot_pattern"),
    ("DENSITY_FUNCTION", "minecraft:worldgen/density_function"),
    (
        "DENSITY_FUNCTION_TYPE",
        "minecraft:worldgen/density_function_type",
    ),
    ("DIALOG", "minecraft:dialog"),
    ("DIALOG_ACTION_TYPE", "minecraft:dialog_action_type"),
    ("DIALOG_BODY_TYPE", "minecraft:dialog_body_type"),
    ("DIALOG_TYPE", "minecraft:dialog_type"),
    ("DIMENSION", "minecraft:dimension"),
    ("DIMENSION_TYPE", "minecraft:dimension_type"),
    ("ENCHANTMENT", "minecraft:enchantment"),
    (
        "ENCHANTMENT_EFFECT_COMPONENT_TYPE",
        "minecraft:enchantment_effect_component_type",
    ),
    (
        "ENCHANTMENT_ENTITY_EFFECT_TYPE",
        "minecraft:enchantment_entity_effect_type",
    ),
    (
        "ENCHANTMENT_LEVEL_BASED_VALUE_TYPE",
        "minecraft:enchantment_level_based_value_type",
    ),
    (
        "ENCHANTMENT_LOCATION_BASED_EFFECT_TYPE",
        "minecraft:enchantment_location_based_effect_type",
    ),
    ("ENCHANTMENT_PROVIDER", "minecraft:enchantment_provider"),
    (
        "ENCHANTMENT_PROVIDER_TYPE",
        "minecraft:enchantment_provider_type",
    ),
    (
        "ENCHANTMENT_VALUE_EFFECT_TYPE",
        "minecraft:enchantment_value_effect_type",
    ),
    (
        "ENTITY_SUB_PREDICATE_TYPE",
        "minecraft:entity_sub_predicate_type",
    ),
    ("ENTITY_TYPE", "minecraft:entity_type"),
    ("ENVIRONMENT_ATTRIBUTE", "minecraft:environment_attribute"),
    ("FEATURE", "minecraft:worldgen/feature"),
    ("FEATURE_SIZE_TYPE", "minecraft:worldgen/feature_size_type"),
    ("FEATURE_TYPE", "minecraft:worldgen/feature_type"),
    (
        "FLAT_LEVEL_GENERATOR_PRESET",
        "minecraft:worldgen/flat_level_generator_preset",
    ),
    ("FLOAT_PROVIDER_TYPE", "minecraft:float_provider_type"),
    ("FLUID", "minecraft:fluid"),
    (
        "FOLIAGE_PLACER_TYPE",
        "minecraft:worldgen/foliage_placer_type",
    ),
    ("FROG_VARIANT", "minecraft:frog_variant"),
    ("GAME_EVENT", "minecraft:game_event"),
    ("GAME_RULE", "minecraft:game_rule"),
    ("HEIGHT_PROVIDER_TYPE", "minecraft:height_provider_type"),
    ("INCOMING_RPC_METHOD", "minecraft:incoming_rpc_methods"),
    ("INPUT_CONTROL_TYPE", "minecraft:input_control_type"),
    ("INSTRUMENT", "minecraft:instrument"),
    ("INT_PROVIDER_TYPE", "minecraft:int_provider_type"),
    ("ITEM", "minecraft:item"),
    ("ITEM_MODIFIER", "minecraft:item_modifier"),
    ("JUKEBOX_SONG", "minecraft:jukebox_song"),
    ("LEVEL_STEM", "minecraft:dimension"),
    ("LOOT_CONDITION_TYPE", "minecraft:loot_condition_type"),
    ("LOOT_FUNCTION_TYPE", "minecraft:loot_function_type"),
    ("LOOT_NBT_PROVIDER_TYPE", "minecraft:loot_nbt_provider_type"),
    ("LOOT_POOL_ENTRY_TYPE", "minecraft:loot_pool_entry_type"),
    (
        "LOOT_SCORE_PROVIDER_TYPE",
        "minecraft:loot_score_provider_type",
    ),
    ("LOOT_TABLE", "minecraft:loot_table"),
    ("MAP_DECORATION_TYPE", "minecraft:map_decoration_type"),
    (
        "MATERIAL_CONDITION",
        "minecraft:worldgen/material_condition",
    ),
    (
        "MATERIAL_CONDITION_TYPE",
        "minecraft:worldgen/material_condition_type",
    ),
    ("MATERIAL_RULE", "minecraft:worldgen/material_rule"),
    (
        "MATERIAL_RULE_TYPE",
        "minecraft:worldgen/material_rule_type",
    ),
    ("MEMORY_MODULE_TYPE", "minecraft:memory_module_type"),
    ("MENU", "minecraft:menu"),
    ("MOB_EFFECT", "minecraft:mob_effect"),
    (
        "MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST",
        "minecraft:worldgen/multi_noise_biome_source_parameter_list",
    ),
    ("NOISE", "minecraft:worldgen/noise"),
    ("NOISE_SETTINGS", "minecraft:worldgen/noise_settings"),
    ("NUMBER_FORMAT_TYPE", "minecraft:number_format_type"),
    ("OUTGOING_RPC_METHOD", "minecraft:outgoing_rpc_methods"),
    ("PAINTING_VARIANT", "minecraft:painting_variant"),
    ("PARTICLE_TYPE", "minecraft:particle_type"),
    ("PERMISSION_CHECK_TYPE", "minecraft:permission_check_type"),
    ("PERMISSION_TYPE", "minecraft:permission_type"),
    ("PIG_SOUND_VARIANT", "minecraft:pig_sound_variant"),
    ("PIG_VARIANT", "minecraft:pig_variant"),
    ("PLACED_FEATURE", "minecraft:worldgen/placed_feature"),
    (
        "PLACEMENT_MODIFIER_TYPE",
        "minecraft:worldgen/placement_modifier_type",
    ),
    ("POINT_OF_INTEREST_TYPE", "minecraft:point_of_interest_type"),
    (
        "POOL_ALIAS_BINDING",
        "minecraft:worldgen/pool_alias_binding",
    ),
    ("POSITION_SOURCE_TYPE", "minecraft:position_source_type"),
    ("POS_RULE_TEST", "minecraft:pos_rule_test"),
    ("POTION", "minecraft:potion"),
    ("PREDICATE", "minecraft:predicate"),
    ("PROCESSOR_LIST", "minecraft:worldgen/processor_list"),
    ("RECIPE", "minecraft:recipe"),
    ("RECIPE_BOOK_CATEGORY", "minecraft:recipe_book_category"),
    ("RECIPE_DISPLAY", "minecraft:recipe_display"),
    ("RECIPE_SERIALIZER", "minecraft:recipe_serializer"),
    ("RECIPE_TYPE", "minecraft:recipe_type"),
    ("ROOT_PLACER_TYPE", "minecraft:worldgen/root_placer_type"),
    (
        "RULE_BLOCK_ENTITY_MODIFIER",
        "minecraft:rule_block_entity_modifier",
    ),
    ("RULE_TEST", "minecraft:rule_test"),
    ("SENSOR_TYPE", "minecraft:sensor_type"),
    ("SLOT_DISPLAY", "minecraft:slot_display"),
    ("SLOT_SOURCE", "minecraft:slot_source"),
    ("SLOT_SOURCE_TYPE", "minecraft:slot_source_type"),
    ("SOUND_EVENT", "minecraft:sound_event"),
    ("SPAWN_CONDITION_TYPE", "minecraft:spawn_condition_type"),
    ("STAT_TYPE", "minecraft:stat_type"),
    ("STRUCTURE", "minecraft:worldgen/structure"),
    ("STRUCTURE_PIECE", "minecraft:worldgen/structure_piece"),
    (
        "STRUCTURE_PLACEMENT",
        "minecraft:worldgen/structure_placement",
    ),
    (
        "STRUCTURE_POOL_ELEMENT",
        "minecraft:worldgen/structure_pool_element",
    ),
    (
        "STRUCTURE_PROCESSOR",
        "minecraft:worldgen/structure_processor",
    ),
    ("STRUCTURE_SET", "minecraft:worldgen/structure_set"),
    ("STRUCTURE_TYPE", "minecraft:worldgen/structure_type"),
    ("SULFUR_CUBE_ARCHETYPE", "minecraft:sulfur_cube_archetype"),
    ("TEMPLATE_POOL", "minecraft:worldgen/template_pool"),
    ("TEST_ENVIRONMENT", "minecraft:test_environment"),
    (
        "TEST_ENVIRONMENT_DEFINITION_TYPE",
        "minecraft:test_environment_definition_type",
    ),
    ("TEST_FUNCTION", "minecraft:test_function"),
    ("TEST_INSTANCE", "minecraft:test_instance"),
    ("TEST_INSTANCE_TYPE", "minecraft:test_instance_type"),
    ("TICKET_TYPE", "minecraft:ticket_type"),
    ("TIMELINE", "minecraft:timeline"),
    ("TRADE_SET", "minecraft:trade_set"),
    (
        "TREE_DECORATOR_TYPE",
        "minecraft:worldgen/tree_decorator_type",
    ),
    ("TRIAL_SPAWNER_CONFIG", "minecraft:trial_spawner"),
    ("TRIGGER_TYPE", "minecraft:trigger_type"),
    ("TRIM_MATERIAL", "minecraft:trim_material"),
    ("TRIM_PATTERN", "minecraft:trim_pattern"),
    ("TRUNK_PLACER_TYPE", "minecraft:worldgen/trunk_placer_type"),
    ("VILLAGER_PROFESSION", "minecraft:villager_profession"),
    ("VILLAGER_TRADE", "minecraft:villager_trade"),
    ("VILLAGER_TYPE", "minecraft:villager_type"),
    ("WOLF_SOUND_VARIANT", "minecraft:wolf_sound_variant"),
    ("WOLF_VARIANT", "minecraft:wolf_variant"),
    ("WORLD_CLOCK", "minecraft:world_clock"),
    ("WORLD_PRESET", "minecraft:worldgen/world_preset"),
    (
        "ZOMBIE_NAUTILUS_VARIANT",
        "minecraft:zombie_nautilus_variant",
    ),
];

/// `net.minecraft.core.registries.BuiltInRegistries` field → registry id.
const BUILT_IN_REGISTRIES: &[(&str, &str)] = &[
    ("ACTIVITY", "minecraft:activity"),
    ("ATTRIBUTE", "minecraft:attribute"),
    ("ATTRIBUTE_TYPE", "minecraft:attribute_type"),
    ("BIOME_SOURCE", "minecraft:worldgen/biome_source"),
    ("BLOCK", "minecraft:block"),
    ("BLOCK_ENTITY_TYPE", "minecraft:block_entity_type"),
    ("BLOCK_PREDICATE_TYPE", "minecraft:block_predicate_type"),
    (
        "BLOCK_STATE_PROVIDER_TYPE",
        "minecraft:worldgen/block_state_provider_type",
    ),
    ("CARVER_TYPE", "minecraft:worldgen/carver_type"),
    ("CHUNK_GENERATOR", "minecraft:worldgen/chunk_generator"),
    ("CHUNK_STATUS", "minecraft:chunk_status"),
    ("COMMAND_ARGUMENT_TYPE", "minecraft:command_argument_type"),
    ("CONSUME_EFFECT_TYPE", "minecraft:consume_effect_type"),
    (
        "CONTEXT_FLOAT_PROVIDER_TYPE",
        "minecraft:context_float_provider_type",
    ),
    (
        "CONTEXT_INT_PROVIDER_TYPE",
        "minecraft:context_int_provider_type",
    ),
    ("CONTEXT_KEY_SET", "minecraft:context_key_set"),
    ("CREATIVE_MODE_TAB", "minecraft:creative_mode_tab"),
    ("CUSTOM_STAT", "minecraft:custom_stat"),
    (
        "DATA_COMPONENT_PREDICATE_TYPE",
        "minecraft:data_component_predicate_type",
    ),
    ("DATA_COMPONENT_TYPE", "minecraft:data_component_type"),
    ("DEBUG_SUBSCRIPTION", "minecraft:debug_subscription"),
    (
        "DENSITY_FUNCTION_TYPE",
        "minecraft:worldgen/density_function_type",
    ),
    ("DIALOG_ACTION_TYPE", "minecraft:dialog_action_type"),
    ("DIALOG_BODY_TYPE", "minecraft:dialog_body_type"),
    ("DIALOG_TYPE", "minecraft:dialog_type"),
    (
        "ENCHANTMENT_EFFECT_COMPONENT_TYPE",
        "minecraft:enchantment_effect_component_type",
    ),
    (
        "ENCHANTMENT_ENTITY_EFFECT_TYPE",
        "minecraft:enchantment_entity_effect_type",
    ),
    (
        "ENCHANTMENT_LEVEL_BASED_VALUE_TYPE",
        "minecraft:enchantment_level_based_value_type",
    ),
    (
        "ENCHANTMENT_LOCATION_BASED_EFFECT_TYPE",
        "minecraft:enchantment_location_based_effect_type",
    ),
    (
        "ENCHANTMENT_PROVIDER_TYPE",
        "minecraft:enchantment_provider_type",
    ),
    (
        "ENCHANTMENT_VALUE_EFFECT_TYPE",
        "minecraft:enchantment_value_effect_type",
    ),
    (
        "ENTITY_SUB_PREDICATE_TYPE",
        "minecraft:entity_sub_predicate_type",
    ),
    ("ENTITY_TYPE", "minecraft:entity_type"),
    ("ENVIRONMENT_ATTRIBUTE", "minecraft:environment_attribute"),
    ("FEATURE_SIZE_TYPE", "minecraft:worldgen/feature_size_type"),
    ("FEATURE_TYPE", "minecraft:worldgen/feature_type"),
    ("FLOAT_PROVIDER_TYPE", "minecraft:float_provider_type"),
    ("FLUID", "minecraft:fluid"),
    (
        "FOLIAGE_PLACER_TYPE",
        "minecraft:worldgen/foliage_placer_type",
    ),
    ("GAME_EVENT", "minecraft:game_event"),
    ("GAME_RULE", "minecraft:game_rule"),
    ("HEIGHT_PROVIDER_TYPE", "minecraft:height_provider_type"),
    ("INCOMING_RPC_METHOD", "minecraft:incoming_rpc_methods"),
    ("INPUT_CONTROL_TYPE", "minecraft:input_control_type"),
    ("INT_PROVIDER_TYPE", "minecraft:int_provider_type"),
    ("ITEM", "minecraft:item"),
    ("LOOT_CONDITION_TYPE", "minecraft:loot_condition_type"),
    ("LOOT_FUNCTION_TYPE", "minecraft:loot_function_type"),
    ("LOOT_NBT_PROVIDER_TYPE", "minecraft:loot_nbt_provider_type"),
    ("LOOT_POOL_ENTRY_TYPE", "minecraft:loot_pool_entry_type"),
    (
        "LOOT_SCORE_PROVIDER_TYPE",
        "minecraft:loot_score_provider_type",
    ),
    ("MAP_DECORATION_TYPE", "minecraft:map_decoration_type"),
    (
        "MATERIAL_CONDITION_TYPE",
        "minecraft:worldgen/material_condition_type",
    ),
    (
        "MATERIAL_RULE_TYPE",
        "minecraft:worldgen/material_rule_type",
    ),
    ("MEMORY_MODULE_TYPE", "minecraft:memory_module_type"),
    ("MENU", "minecraft:menu"),
    ("MOB_EFFECT", "minecraft:mob_effect"),
    ("NUMBER_FORMAT_TYPE", "minecraft:number_format_type"),
    ("OUTGOING_RPC_METHOD", "minecraft:outgoing_rpc_methods"),
    ("PARTICLE_TYPE", "minecraft:particle_type"),
    ("PERMISSION_CHECK_TYPE", "minecraft:permission_check_type"),
    ("PERMISSION_TYPE", "minecraft:permission_type"),
    (
        "PLACEMENT_MODIFIER_TYPE",
        "minecraft:worldgen/placement_modifier_type",
    ),
    ("POINT_OF_INTEREST_TYPE", "minecraft:point_of_interest_type"),
    (
        "POOL_ALIAS_BINDING_TYPE",
        "minecraft:worldgen/pool_alias_binding",
    ),
    ("POSITION_SOURCE_TYPE", "minecraft:position_source_type"),
    ("POS_RULE_TEST", "minecraft:pos_rule_test"),
    ("POTION", "minecraft:potion"),
    ("RECIPE_BOOK_CATEGORY", "minecraft:recipe_book_category"),
    ("RECIPE_DISPLAY", "minecraft:recipe_display"),
    ("RECIPE_SERIALIZER", "minecraft:recipe_serializer"),
    ("RECIPE_TYPE", "minecraft:recipe_type"),
    ("ROOT_PLACER_TYPE", "minecraft:worldgen/root_placer_type"),
    (
        "RULE_BLOCK_ENTITY_MODIFIER",
        "minecraft:rule_block_entity_modifier",
    ),
    ("RULE_TEST", "minecraft:rule_test"),
    ("SENSOR_TYPE", "minecraft:sensor_type"),
    ("SLOT_DISPLAY", "minecraft:slot_display"),
    ("SLOT_SOURCE_TYPE", "minecraft:slot_source_type"),
    ("SOUND_EVENT", "minecraft:sound_event"),
    ("SPAWN_CONDITION_TYPE", "minecraft:spawn_condition_type"),
    ("STAT_TYPE", "minecraft:stat_type"),
    ("STRUCTURE_PIECE", "minecraft:worldgen/structure_piece"),
    (
        "STRUCTURE_PLACEMENT",
        "minecraft:worldgen/structure_placement",
    ),
    (
        "STRUCTURE_POOL_ELEMENT",
        "minecraft:worldgen/structure_pool_element",
    ),
    (
        "STRUCTURE_PROCESSOR",
        "minecraft:worldgen/structure_processor",
    ),
    ("STRUCTURE_TYPE", "minecraft:worldgen/structure_type"),
    (
        "TEST_ENVIRONMENT_DEFINITION_TYPE",
        "minecraft:test_environment_definition_type",
    ),
    ("TEST_FUNCTION", "minecraft:test_function"),
    ("TEST_INSTANCE_TYPE", "minecraft:test_instance_type"),
    ("TICKET_TYPE", "minecraft:ticket_type"),
    (
        "TREE_DECORATOR_TYPE",
        "minecraft:worldgen/tree_decorator_type",
    ),
    ("TRIGGER_TYPES", "minecraft:trigger_type"),
    ("TRUNK_PLACER_TYPE", "minecraft:worldgen/trunk_placer_type"),
    ("VILLAGER_PROFESSION", "minecraft:villager_profession"),
    ("VILLAGER_TYPE", "minecraft:villager_type"),
];

/// The vanilla registry id one of the two registry classes' fields names.
pub(crate) fn registry_id(owner: &str, field: &str) -> Option<&'static str> {
    let table = match owner {
        "net/minecraft/core/registries/Registries" => REGISTRIES_KEYS,
        "net/minecraft/core/registries/BuiltInRegistries" => BUILT_IN_REGISTRIES,
        _ => return None,
    };
    table
        .binary_search_by(|(name, _)| (*name).cmp(field))
        .ok()
        .map(|at| table[at].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_for_lookup() {
        for table in [REGISTRIES_KEYS, BUILT_IN_REGISTRIES] {
            assert!(table.windows(2).all(|pair| pair[0].0 < pair[1].0));
        }
    }

    #[test]
    fn field_names_resolve_to_vanilla_ids() {
        let built_in = "net/minecraft/core/registries/BuiltInRegistries";
        assert_eq!(registry_id(built_in, "FLUID"), Some("minecraft:fluid"));
        assert_eq!(
            registry_id(built_in, "TRIGGER_TYPES"),
            Some("minecraft:trigger_type")
        );
        assert_eq!(
            registry_id("net/minecraft/core/registries/Registries", "BIOME"),
            Some("minecraft:worldgen/biome")
        );
        assert_eq!(registry_id(built_in, "LOGGER"), None);
    }
}

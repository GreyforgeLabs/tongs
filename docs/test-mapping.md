# Test mapping: Python 1.x -> Rust 2.x

Python 1.x is the package released as `atomic-json-store`; Rust 2.x is tongs.

Every pytest case from v1.0.1 has a Rust counterpart. Names keep the Python
test name without the `test_` prefix.

## tests/test_core.py (42 cases)

| Python test | Rust test |
|---|---|
| test_missing_file_yields_default_without_creating_it | tests/store.rs `missing_file_yields_default_without_creating_it` |
| test_default_value_is_copied_not_shared | tests/store.rs `default_value_is_copied_not_shared` |
| test_default_callable_is_invoked | tests/store.rs `default_callable_is_invoked` |
| test_save_and_load_roundtrip_with_envelope | tests/store.rs `save_and_load_roundtrip_with_envelope` |
| test_insertion_order_is_preserved_by_default | tests/store.rs `insertion_order_is_preserved_by_default` |
| test_sort_keys_option | tests/store.rs `sort_keys_option` |
| test_unicode_is_written_verbatim | tests/store.rs `unicode_is_written_verbatim` |
| test_update_with_in_place_mutation | tests/store.rs `update_with_in_place_mutation` |
| test_update_with_replacement_document | tests/store.rs `update_with_replacement_document` |
| test_transaction_commits_on_clean_exit | tests/store.rs `transaction_commits_on_clean_exit` |
| test_transaction_discards_on_exception | tests/store.rs `transaction_discards_on_exception` (closure returns `Err`) |
| test_get_and_set_helpers | tests/store.rs `get_and_set_helpers` |
| test_get_and_set_reject_non_mapping_documents | tests/store.rs `get_and_set_reject_non_mapping_documents` |
| test_reset_restores_default | tests/store.rs `reset_restores_default` |
| test_failed_replace_leaves_original_and_no_temp_files | src/store.rs unit test `failed_replace_leaves_original_and_no_temp_files` (uses a crate-internal fault hook instead of monkeypatching `os.replace`) |
| test_unserializable_data_never_touches_disk | tests/store.rs `unserializable_data_never_touches_disk` (`save_as` with a non-string-keyed map) |
| test_custom_encoder | tests/store.rs `custom_encoder` (serde `Serialize`, `PathBuf` -> string) |
| test_new_file_is_private_and_existing_mode_is_preserved | tests/store.rs `new_file_is_private_and_existing_mode_is_preserved` |
| test_explicit_file_mode | tests/store.rs `explicit_file_mode` |
| test_parent_directories_are_created | tests/store.rs `parent_directories_are_created` |
| test_corrupt_file_raises_by_default | tests/store.rs `corrupt_file_raises_by_default` |
| test_envelope_missing_fields_is_corrupt | tests/store.rs `envelope_missing_fields_is_corrupt` |
| test_quarantine_policy_moves_bad_file_aside | tests/store.rs `quarantine_policy_moves_bad_file_aside` |
| test_legacy_plain_json_is_version_zero | tests/store.rs `legacy_plain_json_is_version_zero` |
| test_migrations_run_in_order_and_persist | tests/store.rs `migrations_run_in_order_and_persist` |
| test_migration_from_legacy_plain_file | tests/store.rs `migration_from_legacy_plain_file` |
| test_missing_migration_step_is_an_error | tests/store.rs `missing_migration_step_is_an_error` |
| test_migration_returning_none_is_an_error | tests/store.rs `migration_returning_none_is_an_error` (also covers `null` and fallible migrations) |
| test_newer_file_version_is_refused | tests/store.rs `newer_file_version_is_refused` |
| test_update_migrates_before_applying | tests/store.rs `update_migrates_before_applying` |
| test_info_reports_metadata_without_migrating | tests/store.rs `info_reports_metadata_without_migrating` |
| test_info_does_not_create_parent_or_lock | tests/store.rs `info_does_not_create_parent_or_lock` |
| test_info_on_corrupt_file | tests/store.rs `info_on_corrupt_file` |
| test_constructor_validation | tests/store.rs `constructor_validation` (negative timeout and empty-name path; the str/negative/non-callable cases are unrepresentable in the typed API) |
| test_lock_file_lives_beside_the_store | tests/store.rs `lock_file_lives_beside_the_store` |
| test_reentrant_lock_within_a_thread | tests/store.rs `reentrant_lock_within_a_thread` |
| test_shared_lock_cannot_upgrade_to_exclusive | tests/store.rs `shared_lock_cannot_upgrade_to_exclusive` (public `lock()` guard replaces `_lock.held`) |
| test_lock_timeout_when_another_thread_holds_the_lock | tests/store.rs `lock_timeout_when_another_thread_holds_the_lock` |
| test_zero_timeout_fails_fast | tests/store.rs `zero_timeout_fails_fast` |
| test_threads_never_lose_increments | tests/store.rs `threads_never_lose_increments` |
| test_processes_never_lose_increments | tests/store.rs `processes_never_lose_increments` (re-executes the test binary as the worker, `process_worker`) |
| test_reader_sees_only_complete_documents_during_concurrent_writes | tests/store.rs `reader_sees_only_complete_documents_during_concurrent_writes` |

## tests/test_cli.py (12 cases)

| Python test | Rust test |
|---|---|
| test_version_flag | tests/cli.rs `version_flag` |
| test_module_entry_point | tests/cli.rs `module_entry_point` (the binary replaces `python -m`) |
| test_init_creates_store_and_is_idempotent | tests/cli.rs `init_creates_store_and_is_idempotent` |
| test_info_on_missing_and_existing | tests/cli.rs `info_on_missing_and_existing` |
| test_set_get_dump_delete_flow | tests/cli.rs `set_get_dump_delete_flow` |
| test_set_respects_existing_schema_version | tests/cli.rs `set_respects_existing_schema_version` |
| test_missing_key_exit_code_and_default | tests/cli.rs `missing_key_exit_code_and_default` |
| test_missing_store_is_an_error_for_reads | tests/cli.rs `missing_store_is_an_error_for_reads` |
| test_invalid_json_value_is_a_usage_error | tests/cli.rs `invalid_json_value_is_a_usage_error` |
| test_corrupt_store_is_an_error | tests/cli.rs `corrupt_store_is_an_error` |
| test_legacy_plain_file_can_be_edited | tests/cli.rs `legacy_plain_file_can_be_edited` |
| test_path_helpers | src/cli/keypath.rs unit test `path_helpers` |

## Added in 2.0.0

- tests/store.rs: `on_disk_format_is_python_compatible`, `compact_ascii_and_sorted_options`, `big_integers_and_non_finite_numbers_survive_rewrites`, `transaction_returns_closure_value_and_lock_is_released`, `deep_documents_are_corrupt_not_a_crash`.
- tests/cli.rs: argparse parity (`no_arguments_is_a_usage_error`, `unknown_command_lists_choices`, `subcommand_errors_use_the_subcommand_usage`, `unrecognized_and_invalid_options`, `abbreviations_and_negative_numbers`, `help_output_matches_argparse`), `list_indexes_and_python_int_semantics`, `info_and_reads_never_write_the_store`, `lock_timeout_reports_python_message`, `unreadable_envelope_and_os_errors`, `unicode_decimal_digits_follow_python_int_and_float`, `non_utf8_arguments_are_reported_like_python`, `deeply_nested_files_are_refused_without_crashing`; rename coverage: `deprecated_alias_prints_one_note_then_behaves_like_tongs`, `envelope_on_disk_keeps_the_python_1x_format_string`; interrupt safety: `interrupt_during_a_write_leaves_no_temp_file_and_no_torn_store` (real `SIGINT`/`SIGTERM`/`SIGHUP` sent while the CLI's temporary file exists).
- tests/differential.rs (run with `AJS_PYTHON_REF=<v1.0.1>/src`): `cli_matches_python_byte_for_byte`, `files_written_by_either_implementation_are_identical`, `mixed_processes_never_lose_increments` (Python and Rust processes incrementing one store), `python_lock_blocks_rust_and_rust_lock_blocks_python`, `prog_normalisation_only_touches_the_program_name` (CLI output is compared with `atomic-json-store` and `tongs` normalised to one program name).
- Unit tests: CPython-compatible JSON decoding errors, number canonicalisation and output formatting (`src/json.rs`), `repr()`/pathlib/`float()`/`int()` emulation (`src/python.rs`), date conversion (`src/sys.rs`), the pinned `atomic-json-store/1` format marker and signal deferral during publish (`src/store.rs` `format_marker_is_the_python_1x_value`, `signal_during_publish_is_deferred_until_the_write_completes`), textwrap and help formatting (`src/cli/argparse.rs`, `src/cli/mod.rs`), Python list-index semantics (`src/cli/keypath.rs`).

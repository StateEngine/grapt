"""Nightly-only test tags. Keep names exact; never skip an entire module."""
from pathlib import Path
import re

# Exact names measured above the roughly 60-second per-test policy on the
# governed workstation (cache-lineage R3 serial run, 2026-10-26). Keep
# their real durable-write bounds in nightly rather than shrink them; a new
# slow measurement is a new tag here, never a module skip.
TESTS = {
    'grapt-connections': {
        # Native Windows credential persistence exceeds the per-test budget.
        'store::tests::key_install_recovers_each_side_of_the_metadata_commit_without_resurrection':
            'crates/grapt-connections/src/store/tests.rs',
        'store::tests::key_install_distinguishes_visible_commits_and_rejects_oauth_before_writes':
            'crates/grapt-connections/src/store/tests.rs',
        'store::tests::removal_recovers_both_sides_of_commit_without_restoring_slots_or_staging':
            'crates/grapt-connections/src/store/tests.rs',
        'chatgpt::tests::a_migrated_sign_in_updates_the_managed_account_and_retains_user_configuration':
            'crates/grapt-connections/src/chatgpt/tests.rs',
    },
    'grapt-platform': {
        'credential::renewal::tests::vaulted_credentials_keep_their_binding_on_initial_and_renewed_capabilities':
            'crates/grapt-platform/src/credential/renewal/tests.rs',
    },
    'grapt-cli': {
        # Full qualification measured the real production stall bound at 151 seconds.
        'server_loss::a_run_whose_server_goes_silent_after_admission_exits_remote_within_the_bound':
            'crates/grapt-cli/tests/public_seams/server_loss.rs',
    },
    'grapt-product': {
        # Large durable histories exceed the per-test runtime bound.
        'message_page_bytes::api01_large_tool_history_pages_by_encoded_bytes_without_losing_rows':
            'crates/grapt-product/tests/dogfood_headless/message_page_bytes.rs',
        'declared_crossing::promoted_inputs_past_the_byte_bound_compact_reopen_and_continue':
            'crates/grapt-product/tests/dogfood_headless/declared_crossing.rs',
        'dogfood::native_mail::custody::wake::tests::service::interleaving::normal_provider_second_input_with_original_debt':
            'crates/grapt-product/src/dogfood/native_mail/custody/wake/tests/interleaving.rs',
        # The measured test-class ruling, 2026-11-04.
        'cache_usage_benchmark::million_attempt_benchmark_executes_keyed_comparisons_for_exact_hit_and_miss_halves':
            'crates/grapt-product/tests/cache_usage_benchmark.rs',
        'sustained_allowance::structural::swr_j03_structural_call_bound_compacts_and_rebuilds_the_ordinary_request':
            'crates/grapt-product/tests/dogfood_headless/sustained_allowance/structural.rs',
        'sustained_allowance::structural::swr_j03_promoted_and_event_bounds_each_run_the_actual_compactor':
            'crates/grapt-product/tests/dogfood_headless/sustained_allowance/structural.rs',
        'dogfood::continuation::tests::accumulation::completed_effects_do_not_limit_session_lifetime_or_target_history':
            'crates/grapt-product/src/dogfood/continuation/tests/accumulation.rs',
        'dogfood::continuation::tests::accumulation::pre_compaction_unresolved_effects_still_quarantine_after_restart':
            'crates/grapt-product/src/dogfood/continuation/tests/accumulation.rs',
        'dogfood::continuation::tests::history_page::lifetime::a_selected_compaction_reads_past_128_prior_descriptors_in_bounded_batches':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page_long.rs',
        'dogfood::continuation::tests::history_page::lifetime::more_than_128_inputs_failures_and_notices_page_end_to_end_with_compactions':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page_long.rs',
        'dogfood::continuation::tests::response_groups::context_capacity::sc_rel_07a_a_response_group_overflow_is_an_item_capacity':
            'crates/grapt-product/src/dogfood/continuation/tests/context_capacity.rs',
        'dogfood::continuation::tests::response_groups::context_capacity::sc_rel_07a_a_summary_and_retained_overflow_compacts_instead_of_refusing_the_plan':
            'crates/grapt-product/src/dogfood/continuation/tests/context_capacity.rs',
        'dogfood::continuation::tests::response_groups::response_boundaries_participate_in_structural_compaction_admission':
            'crates/grapt-product/src/dogfood/continuation/tests/response_groups.rs',
        'dogfood::continuation::tests::split_exchange::a_structural_bound_between_exchanges_is_unchanged':
            'crates/grapt-product/src/dogfood/continuation/tests/split_exchange.rs',
        'dogfood::continuation::tests::split_exchange::a_structural_bound_inside_an_exchange_summarizes_below_it':
            'crates/grapt-product/src/dogfood/continuation/tests/split_exchange.rs',
        'dogfood::continuation::tests::split_exchange::one_split_exchange_with_nothing_before_it_refuses_without_an_empty_summary':
            'crates/grapt-product/src/dogfood/continuation/tests/split_exchange.rs',
        'dogfood::continuation::tests::structural_bounds::many_large_outputs_refuse_before_their_payloads_are_read':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_bounds.rs',
        'dogfood::continuation::tests::sustained_defects::swr_c1_tiny_settled_calls_compact_before_the_page_bound':
            'crates/grapt-product/src/dogfood/continuation/tests/sustained_defects.rs',
        'dogfood::continuation::tests::sustained_defects::swr_c2_a_tight_cut_suffix_advances_through_another_compaction':
            'crates/grapt-product/src/dogfood/continuation/tests/sustained_defects.rs',
        'terminal_compaction::provider_watchdog::rel14_long_write_stdin_poll_completes_without_killing_the_execution':
            'crates/grapt-product/tests/dogfood_headless/terminal_compaction/provider_watchdog.rs',
        # A ten-minute busy seat: the TUI under streaming, gaps, stalls and restarts.
        'tui_busy_seat::a_busy_seat_soak_keeps_input_and_recovers_for_ten_minutes':
            'crates/grapt-product/tests/dogfood_headless/tui_busy_seat.rs',
        # Durable continuation histories measured above the 60-second per-test policy.
        'dogfood::continuation::tests::background::unrelated_reads_keep_recovery_and_a_later_final_poll_removes_it':
            'crates/grapt-product/src/dogfood/continuation/tests/background.rs',
        'dogfood::continuation::tests::history_batches::a_render_batch_past_one_group_projects_every_call_through_shared_reads':
            'crates/grapt-product/src/dogfood/continuation/tests/history_batches.rs',
        'dogfood::continuation::tests::history_batches::br_h_batch_keeps_outcome_arguments_withholding_capture_and_restart_truth':
            'crates/grapt-product/src/dogfood/continuation/tests/history_batches.rs',
        'dogfood::continuation::tests::history_batches::br_h_batch_retains_nested_progress_files_order_and_restart':
            'crates/grapt-product/src/dogfood/continuation/tests/history_batches.rs',
        'dogfood::continuation::tests::history_batches::br_h_grouped_calls_cross_the_eight_call_bound_without_individual_fallback':
            'crates/grapt-product/src/dogfood/continuation/tests/history_batches.rs',
        'dogfood::continuation::tests::history_batches::br_h_public_batch_preserves_eight_views_with_comparable_work_measurements':
            'crates/grapt-product/src/dogfood/continuation/tests/history_batches.rs',
        'dogfood::continuation::tests::history_page::a_later_unreadable_compaction_spares_earlier_pages':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page.rs',
        'dogfood::continuation::tests::history_page::a_one_row_page_of_a_long_history_renders_only_its_row':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page.rs',
        'dogfood::continuation::tests::history_page::br_h_prefetched_tool_refusals_wait_for_their_selected_row':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page.rs',
        'dogfood::continuation::tests::history_page::compaction_history_exposes_only_supplied_admission_token_estimates':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page.rs',
        'dogfood::continuation::tests::history_page::lifetime::api01_a_page_replays_each_compaction_lineage_step_once':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page_long.rs',
        'dogfood::continuation::tests::history_page::lifetime::duplicate_sequences_on_the_next_page_boundary_refuse_instead_of_disappearing':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page_long.rs',
        'dogfood::continuation::tests::history_page::lifetime::malformed_old_message_facts_are_named_only_on_pages_that_select_the_message':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page_long.rs',
        'dogfood::continuation::tests::history_page::lifetime::malformed_or_oversized_old_notices_spare_new_pages_and_keep_unknown_diagnostics':
            'crates/grapt-product/src/dogfood/continuation/tests/history_page_long.rs',
        'dogfood::continuation::tests::history_page::omissions::a_descending_page_places_each_gap_on_the_row_that_owns_it':
            'crates/grapt-product/src/dogfood/continuation/tests/history_omissions.rs',
        'dogfood::continuation::tests::history_page::omissions::an_assistant_page_marks_unreadable_content':
            'crates/grapt-product/src/dogfood/continuation/tests/history_omissions.rs',
        'dogfood::continuation::tests::history_page::omissions::an_omission_does_not_rewrite_the_users_words':
            'crates/grapt-product/src/dogfood/continuation/tests/history_omissions.rs',
        'dogfood::continuation::tests::history_page::omissions::an_unreadable_opening_spares_other_public_messages':
            'crates/grapt-product/src/dogfood/continuation/tests/history_omissions.rs',
        'dogfood::continuation::tests::history_page::read_isolation::a_failed_async_projection_does_not_recycle_its_reader_into_the_pool':
            'crates/grapt-product/src/dogfood/continuation/tests/history_read_isolation.rs',
        'dogfood::continuation::tests::history_page::read_isolation::abandoning_an_active_sql_read_releases_capacity_without_poisoning_reuse':
            'crates/grapt-product/src/dogfood/continuation/tests/history_read_isolation.rs',
        'dogfood::continuation::tests::nested::a_long_completed_nested_history_stays_within_the_orphan_bound':
            'crates/grapt-product/src/dogfood/continuation/tests/nested.rs',
        'dogfood::continuation::tests::original_users::lifetime_history_beyond_one_page_continues_after_compacted_restart':
            'crates/grapt-product/src/dogfood/continuation/tests/original_users.rs',
        'dogfood::continuation::tests::structural_bounds::history_past_the_read_bounds_summarizes_its_newest_window':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_bounds.rs',
        'dogfood::continuation::tests::structural_bounds::the_promoted_page_bound_plans':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_bounds.rs',
        'dogfood::continuation::tests::structural_bounds::the_tool_call_page_bound_plans_and_one_more_is_structural':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_bounds.rs',
        'dogfood::continuation::tests::structural_bounds::the_tool_probe_counts_a_malformed_row_past_the_page_bound':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_bounds.rs',
        'dogfood::continuation::tests::structural_bounds::w1_two_hundred_small_calls_compact_only_on_tokens':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_bounds.rs',
        'dogfood::continuation::tests::structural_request::a_reused_request_never_covers_inputs_that_arrived_after_it':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_request.rs',
        'dogfood::continuation::tests::structural_request::a_structural_refusal_hands_the_compactor_the_request_last_sent':
            'crates/grapt-product/src/dogfood/continuation/tests/structural_request.rs',
        'dogfood::continuation::tests::sustained_defects::swr_c1_promoted_inputs_compact_before_the_page_bound':
            'crates/grapt-product/src/dogfood/continuation/tests/sustained_defects.rs',
    },
    'grapt-artifact': {
        'the_pooled_structural_check_reports_the_first_failure_in_reference_order':
            'crates/grapt-artifact/tests/carry_structural.rs',
    },
    'grapt-inbox': {
        'full_page_reads_two_hundred_distinct_admissions':
            'crates/grapt-inbox/tests/admission_coordinates.rs',
    },
    'grapt-provider': {
        'provider_probes_are_rejected_by_the_compiler':
            'crates/grapt-provider/tests/compile_fail.rs',
    },
    'grapt-runtime': {
        'runtime_probes_are_rejected_by_the_compiler':
            'crates/grapt-runtime/tests/compile_fail.rs',
    },
    'grapt-store': {
        'session_event_queries::batch_tests::br_h_batch_vm_work_improves_selected_calls_without_a_new_index':
            'crates/grapt-store/src/session_event_queries/batch_tests.rs',
    },
    'grapt-client': {
        # Full qualification measured these server-fixture journeys above 60 seconds.
        'compaction_label::real_compaction_completed_label_and_summary_survive_client_restart':
            'crates/grapt-client/tests/dogfood_journey/compaction_label.rs',
        'directory_placement::directory_launch_keeps_non_git_and_nested_git_session_placement':
            'crates/grapt-client/tests/dogfood_journey/directory_placement.rs',
        'live_stream::a_subscription_opened_before_the_session_still_delivers_live_frames':
            'crates/grapt-client/tests/dogfood_journey/live_stream.rs',
        'live_stream::the_real_stream_delivers_frames_and_a_drop_reconciles_from_durable_truth':
            'crates/grapt-client/tests/dogfood_journey/live_stream.rs',
        'live_stream::the_real_transport_carries_the_query_the_request_declares':
            'crates/grapt-client/tests/dogfood_journey/live_stream.rs',
    },
}


def arguments(packages=None):
    selected = TESTS if packages is None else set(packages)
    skipped = [arg for package in sorted(selected) for name in TESTS.get(package, {}) for arg in ('--skip', name)]
    return ['--exact', *skipped] if skipped else []


def validate(root):
    """A renamed/deleted tagged witness must fail policy rather than silently vanish."""
    for tests in TESTS.values():
        for name, source in tests.items():
            path = root/source
            if not path.is_file() or path.is_symlink() or path.stat().st_size > 512*1024:
                raise ValueError('Nightly test source is missing or oversized: '+source)
            function = name.rsplit('::', 1)[-1]
            attribute = (r'#\[(?:test|tokio::test(?:\([^\]]*\))?)\]\s*'
                         r'(?:#\[[^\]]*\]\s*)*(?:async\s+)?fn\s+')
            if len(re.findall(attribute+re.escape(function)+r'\s*\(', path.read_text())) != 1:
                raise ValueError('Nightly test tag does not identify one source test: '+name)


def skip_names(command):
    """Exact names following each --skip in a flat cargo argument list."""
    if command[:3]==['cargo','nextest','run'] and '-E' in command:
        if command.count('-E')!=1 or command.index('-E')+1>=len(command):
            raise ValueError('Invalid nextest exact-skip filter')
        expression=command[command.index('-E')+1]
        names=re.findall(r'test\(=([A-Za-z0-9_:]{1,1024})\)',expression)
        if not names or len(names)>256 or expression!='not ('+' | '.join('test(='+name+')' for name in names)+')':
            raise ValueError('Invalid nextest exact-skip filter')
        return names
    return [command[index + 1] for index in range(len(command) - 1)
            if command[index] == '--skip']


def _without_filter(command):
    """The command without its exact/skip test filter."""
    kept = []
    index = 0
    while index < len(command):
        argument = command[index]
        if argument == '--exact':
            index += 1
        elif (argument == '--skip' or command[:3]==['cargo','nextest','run'] and argument=='-E') and index + 1 < len(command):
            index += 2
        else:
            kept.append(argument)
            index += 1
    return kept


def equivalent(recorded, required):
    """A command recorded under an older tag set still covers the current
    policy when it ran at least every test the policy still runs."""
    if recorded == required:
        return True
    if not (isinstance(recorded,list) and isinstance(required,list)):
        return False
    if recorded[:3]==required[:3]==['cargo','nextest','run']:
        try:
            return set(skip_names(recorded))<=set(skip_names(required)) and _without_filter(recorded)==_without_filter(required)
        except ValueError:
            return False
    if recorded[:2]!=required[:2] or recorded[:2]!=['cargo','test']:
        return False
    if '--skip' in recorded:
        # Substring skips prove no coverage; only exact names are admissible.
        if '--exact' not in recorded or '--exact' not in required:
            return False
        if not set(skip_names(recorded)) <= set(skip_names(required)):
            return False
    return _without_filter(recorded) == _without_filter(required)

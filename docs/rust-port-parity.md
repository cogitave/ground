# Rust port parity

The TypeScript M1 under `src/` is the behavioural reference. Each row maps one
`it()` block to the Rust test with the same intent (92 rows; `it.each` and the
approval loop are one row each because the Rust side tables them in one test, so
the 93 literal `it()` blocks expand to 107 cases, all asserted). A TS scenario with no Rust counterpart
would be a gap; there are none.

## `src/adapters/adapters.test.ts`

| TypeScript test                                                             | Rust test                                                                                        |
| --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| mapPr: maps the fields                                                      | `adapters::github_rest::tests::map_pr_maps_the_fields`                                           |
| mapPr: survives a deleted fork and derives merged from merged_at            | `adapters::github_rest::tests::map_pr_survives_a_deleted_fork_and_derives_merged_from_merged_at` |
| mapFile/mapJob/mapRun: normalises file status, patch only when present      | `adapters::github_rest::tests::map_file_normalises_status_and_keeps_the_patch_only_when_present` |
| mapFile/mapJob/mapRun: maps jobs                                            | `adapters::github_rest::tests::map_job_maps_jobs`                                                |
| mapFile/mapJob/mapRun: maps runs, reducing the workflow path to a file name | `adapters::github_rest::tests::map_run_reduces_the_workflow_path_to_a_file_name`                 |
| gh helpers: builds the admin merge command                                  | `adapters::github_rest::tests::builds_the_admin_merge_command`                                   |
| gh helpers: reads the token through the injected exec                       | `adapters::exec::tests::reads_the_token_through_the_injected_exec`                               |
| gh helpers: shells out to gh for an admin merge and reads the merge commit  | `adapters::github_rest::tests::shells_out_to_gh_for_an_admin_merge_and_reads_the_merge_commit`   |
| npm registry: reads dist-tags.latest                                        | `adapters::registry_npm::tests::reads_dist_tags_latest`                                          |
| npm registry: escapes scoped names                                          | `adapters::registry_npm::tests::escapes_scoped_names`                                            |
| npm registry: maps 404 to null, errors to throws, success to the version    | `adapters::registry_npm::tests::maps_404_to_none_errors_to_errors_and_success_to_the_version`    |
| SystemClock: reports a real date                                            | `adapters::clock_system::tests::reports_a_real_date`                                             |

## `src/approval.test.ts`

| TypeScript test                                                                                | Rust test                      |
| ---------------------------------------------------------------------------------------------- | ------------------------------ |
| canApproveRun: all 9 rows (allows/refuses per case, incl. case-insensitive slug and closed PR) | `approval::tests::truth_table` |

## `src/cli/doctor.test.ts`

| TypeScript test                                                    | Rust test                                                                               |
| ------------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| exits 0 when every check passes                                    | `cli::doctor::tests::exits_0_when_every_check_passes`                                   |
| exits 1 on a failing or throwing check, and keeps running the rest | `cli::doctor::tests::exits_1_on_a_failing_or_throwing_check_and_keeps_running_the_rest` |
| treats a failing warn check as non-fatal                           | `cli::doctor::tests::a_failing_warn_check_is_non_fatal`                                 |
| prints a table with one line per check                             | `cli::doctor::tests::prints_a_table_with_one_line_per_check`                            |

## `src/cli/install-service.test.ts`

| TypeScript test                                                                | Rust test                                                                                       |
| ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------- |
| writes the unit under <configHome>/systemd/user and returns the enable command | `cli::install_service::tests::writes_the_unit_under_config_home_and_returns_the_enable_command` |
| quotes paths with spaces and carries a custom GROUND_HOME                      | `cli::install_service::tests::quotes_paths_with_spaces_and_carries_a_custom_ground_home`        |

## `src/cli/ship-command.test.ts`

| TypeScript test                                           | Rust test                                                                                 |
| --------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| prints the plan and exits 0 on a dry run, writing nothing | `lanes::release::command_tests::prints_the_plan_and_exits_0_on_a_dry_run_writing_nothing` |
| ships with --confirm and exits 0                          | `lanes::release::command_tests::ships_with_confirm_and_exits_0`                           |
| prints the outcome as JSON with --json                    | `lanes::release::command_tests::prints_the_outcome_as_json_with_json`                     |
| exits non-zero when CI fails for good                     | `lanes::release::command_tests::exits_non_zero_when_ci_fails_for_good`                    |

## `src/config.test.ts`

| TypeScript test                                                                                                                                           | Rust test                                                            |
| --------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- |
| accepts the design example                                                                                                                                | `config::tests::accepts_the_design_example`                          |
| it.each: 7 bad values (repo, mergeMethod, pollSeconds=0, maxRerunsPerRun=1.5, adminMerge, approveBranches=[1], ciWorkflow=''), each message names the key | `config::tests::rejects_bad_values_naming_the_key`                   |
| rejects a missing key and names it                                                                                                                        | `config::tests::rejects_a_missing_key_and_names_it`                  |
| refuses approving bot runs with an empty allow-list                                                                                                       | `config::tests::refuses_approving_bot_runs_with_an_empty_allow_list` |
| rejects non-objects                                                                                                                                       | `config::tests::rejects_non_objects_and_unknown_keys`                |
| loadRepoConfig: loads a file / names the file on invalid JSON and on a missing file                                                                       | `config::tests::loads_a_file_and_names_it_on_failure`                |

## `src/daemon/server.test.ts`

| TypeScript test                                         | Rust test                                                                       |
| ------------------------------------------------------- | ------------------------------------------------------------------------------- |
| is private to the user                                  | `daemon::server::tests::the_socket_is_private_to_the_user`                      |
| answers status                                          | `daemon::server::tests::answers_status`                                         |
| answers events, newest first and limited                | `daemon::server::tests::answers_events_newest_first_and_limited`                |
| refuses unknown commands, unknown repos and non-objects | `daemon::server::tests::refuses_unknown_commands_unknown_repos_and_non_objects` |
| replaces a stale socket file but not a live daemon      | `daemon::server::tests::replaces_a_stale_socket_file_but_not_a_live_daemon`     |

## `src/daemon/watcher.test.ts`

| TypeScript test                                              | Rust test                                                                             |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------- |
| records the tip once and again only when it moves            | `daemon::watcher::tests::records_the_tip_once_and_again_only_when_it_moves`           |
| records version PR appearance, head change and disappearance | `daemon::watcher::tests::records_version_pr_appearance_head_change_and_disappearance` |
| records a run only when its state changes                    | `daemon::watcher::tests::records_a_run_only_when_its_state_changes`                   |
| does not repeat events after a restart                       | `daemon::watcher::tests::does_not_repeat_events_after_a_restart`                      |
| logs a failing poll once and keeps going                     | `daemon::watcher::tests::logs_a_failing_poll_once_and_keeps_going`                    |
| approves an action_required run for the PR head, once        | `daemon::watcher::tests::approves_an_action_required_run_for_the_pr_head_once`        |
| does nothing when the repo has not opted in                  | `daemon::watcher::tests::does_nothing_when_the_repo_has_not_opted_in`                 |
| never approves a fork head                                   | `daemon::watcher::tests::never_approves_a_fork_head`                                  |
| never approves a run for an older SHA                        | `daemon::watcher::tests::never_approves_a_run_for_an_older_sha`                       |
| run() stops when the stop promise resolves                   | `daemon::watcher::tests::run_stops_when_the_stop_signal_fires`                        |

## `src/lanes/release/bumps.test.ts`

| TypeScript test                                                                     | Rust test                                                                                                        |
| ----------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| reads name and versions from a standard hunk                                        | `lanes::release::bumps_tests::reads_name_and_versions_from_a_standard_hunk`                                      |
| ignores nested versions, private packages, non-package files and unchanged versions | `lanes::release::bumps_tests::ignores_nested_versions_private_packages_non_package_files_and_unchanged_versions` |
| reports a package whose name is not in the diff instead of guessing                 | `lanes::release::bumps_tests::reports_a_package_whose_name_is_not_in_the_diff_instead_of_guessing`               |
| treats a new package as a bump from nothing                                         | `lanes::release::bumps_tests::treats_a_new_package_as_a_bump_from_nothing`                                       |

## `src/lanes/release/ship.test.ts`

| TypeScript test                                                                 | Rust test                                                                                                     |
| ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| dry-run stops before the first write and leaves the session resumable           | `lanes::release::ship_tests::dry_run_stops_before_the_first_write_and_leaves_the_session_resumable`           |
| with confirm merges once at the checked head and confirms the registry          | `lanes::release::ship_tests::with_confirm_merges_once_at_the_checked_head_and_confirms_the_registry`          |
| reports that the base moved when it is no longer the tip                        | `lanes::release::ship_tests::reports_that_the_base_moved_when_it_is_no_longer_the_tip`                        |
| reports nothing to ship without pending changesets                              | `lanes::release::ship_tests::reports_nothing_to_ship_without_pending_changesets`                              |
| reports nothing to ship when changesets are pending but no release run is going | `lanes::release::ship_tests::reports_nothing_to_ship_when_changesets_are_pending_but_no_release_run_is_going` |
| waits for the running release to open the PR, then ships                        | `lanes::release::ship_tests::waits_for_the_running_release_to_open_the_pr_then_ships`                         |
| waits for a stale PR to be refreshed, then gates the new head                   | `lanes::release::ship_tests::waits_for_a_stale_pr_to_be_refreshed_then_gates_the_new_head`                    |
| times out, resumably, when the PR is never refreshed                            | `lanes::release::ship_tests::snapshot_times_out_resumably_when_the_pr_is_never_refreshed`                     |
| approves the current head when enabled                                          | `lanes::release::ship_tests::approves_the_current_head_when_enabled`                                          |
| plans the approval in a dry run without writing                                 | `lanes::release::ship_tests::plans_the_approval_in_a_dry_run_without_writing`                                 |
| does not approve when disabled and reports it as blocked                        | `lanes::release::ship_tests::does_not_approve_when_disabled_and_reports_it_as_blocked`                        |
| ignores an action_required run for an older head                                | `lanes::release::ship_tests::ignores_an_action_required_run_for_an_older_head`                                |
| does nothing while only an older head has a run                                 | `lanes::release::ship_tests::does_nothing_while_only_an_older_head_has_a_run`                                 |
| never approves a fork head, even with approval enabled                          | `lanes::release::ship_tests::never_approves_a_fork_head_even_with_approval_enabled`                           |
| never approves a run whose branch differs from the PR head branch               | `lanes::release::ship_tests::never_approves_a_run_whose_branch_differs_from_the_pr_head_branch`               |
| never approves a branch outside the allow-list                                  | `lanes::release::ship_tests::never_approves_a_branch_outside_the_allow_list`                                  |
| does not re-run twice while GitHub still lists the old attempt                  | `lanes::release::ship_tests::does_not_re_run_twice_while_github_still_lists_the_old_attempt`                  |
| re-runs once, records the flake, then stops with the failing jobs               | `lanes::release::ship_tests::re_runs_once_records_the_flake_then_stops_with_the_failing_jobs`                 |
| ships when the re-run passes, and says so                                       | `lanes::release::ship_tests::ships_when_the_re_run_passes_and_says_so`                                        |
| plans the re-run in a dry run without writing                                   | `lanes::release::ship_tests::plans_the_re_run_in_a_dry_run_without_writing`                                   |
| re-runs a failed release once, then reports the failure                         | `lanes::release::ship_tests::re_runs_a_failed_release_once_then_reports_the_failure`                          |
| aborts without merging when the head moved after CI passed                      | `lanes::release::ship_tests::aborts_without_merging_when_the_head_moved_after_ci_passed`                      |
| registry: tolerates propagation lag                                             | `lanes::release::ship_tests::tolerates_propagation_lag`                                                       |
| registry: reports, without throwing, a version the registry never shows         | `lanes::release::ship_tests::reports_without_failing_a_version_the_registry_never_shows`                      |
| resume: continues a CI wait without approving twice                             | `lanes::release::ship_tests::continues_a_ci_wait_without_approving_twice`                                     |
| resume: continues the release wait after the merge without merging again        | `lanes::release::ship_tests::continues_the_release_wait_after_the_merge_without_merging_again`                |

## `src/state/store.test.ts`

| TypeScript test                                                   | Rust test                                                                               |
| ----------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| creates, updates and finds the active session                     | `state::store::tests::creates_updates_and_finds_the_active_session`                     |
| stops being active once it has an outcome, and is per repo        | `state::store::tests::stops_being_active_once_it_has_an_outcome_and_is_per_repo`        |
| returns the newest active session                                 | `state::store::tests::returns_the_newest_active_session`                                |
| rejects updates to unknown sessions                               | `state::store::tests::rejects_updates_to_unknown_sessions`                              |
| lists events newest first, honouring the limit                    | `state::store::tests::lists_events_newest_first_honouring_the_limit`                    |
| records flakes per repo                                           | `state::store::tests::records_flakes_per_repo`                                          |
| creates a 0700 directory, migrates once and persists across opens | `state::store::tests::on_disk_store_is_private_migrates_once_and_persists_across_opens` |

## `src/testing/testing.test.ts`

| TypeScript test                                                             | Rust test                                                                               |
| --------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| FakeClock advances virtual time on sleep without waiting and records sleeps | `testing::tests::fake_clock_advances_virtual_time_on_sleep_and_records_sleeps`          |
| FakeRegistry returns null for unknown packages and records queries          | `testing::tests::fake_registry_returns_none_for_unknown_packages_and_records_queries`   |
| FakeRegistry publishes after the scheduled number of queries                | `testing::tests::fake_registry_publishes_after_the_scheduled_number_of_queries`         |
| MemoryLogger keeps entries by level                                         | `testing::tests::memory_logger_keeps_entries_by_level`                                  |
| FakeGitHub finds open PRs by head and base and ignores closed ones          | `testing::tests::fake_github_finds_open_prs_by_head_and_base_and_ignores_closed_ones`   |
| FakeGitHub serves files, dirs and branch shas                               | `testing::tests::fake_github_serves_files_dirs_and_branch_shas`                         |
| FakeGitHub advances a run one stage per poll and filters                    | `testing::tests::fake_github_advances_a_run_one_stage_per_poll_and_filters`             |
| FakeGitHub records approvals and re-runs and swaps in their scripts         | `testing::tests::fake_github_records_approvals_and_reruns_and_swaps_in_their_scripts`   |
| FakeGitHub merges only with the expected head sha and fires onMerge         | `testing::tests::fake_github_merges_only_with_the_expected_head_sha_and_fires_on_merge` |
| FakeGitHub lets a test move the head between reads                          | `testing::tests::fake_github_lets_a_test_move_the_head_between_reads`                   |
| FakeGitHub serves scripted jobs                                             | `testing::tests::fake_github_serves_scripted_jobs`                                      |

## Rust-only tests

These pin behaviour the Rust port added or made explicit.

| Rust test                                                                                                                                                  | Why                                                                 |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| `config::tests::integers_accept_whole_floats_only`                                                                                                         | JSON numbers are floats; `5.0` is an integer, `1.5` is not          |
| `config::tests::slugs_need_exactly_owner_and_name`                                                                                                         | slug validation table                                               |
| `state::store::tests::tightens_a_pre_existing_loose_directory`                                                                                             | the state directory is forced to 0700                               |
| `state::store::tests::a_patch_can_clear_the_outcome_and_the_pr_number`                                                                                     | explicit `Option<Option<_>>` patch semantics                        |
| `lanes::release::ship_tests::outcome_serialises_with_camel_case_keys`                                                                                      | `--json` wire shape                                                 |
| `daemon::server::tests::answers_garbage_with_an_error_and_keeps_the_connection`                                                                            | bad JSON gets an error, not a closed socket                         |
| `daemon::server::tests::refuses_an_overlong_request_line_and_closes`                                                                                       | bounded request line                                                |
| `daemon::server::tests::will_not_delete_a_non_socket_at_the_socket_path`                                                                                   | stale-socket cleanup only removes sockets                           |
| `daemon::watcher::tests::run_wakes_from_its_sleep_when_stopped`                                                                                            | graceful stop does not wait out a poll interval                     |
| `daemon::watcher::tests::never_merges_even_with_a_green_approved_pr`                                                                                       | hard rule 3: the daemon never merges                                |
| `adapters::github_rest::tests::a_non_json_success_body_names_the_status_and_the_body`                                                                      | a gateway page is reported, not read as `null`                      |
| `adapters::github_rest::tests::*` (REST routes, headers, errors), `daemon::paths`, `daemon::repos`, `daemon::console_logger`, `daemon::daemon`, `cli::run` | the Rust adapters and CLI wiring have no TS unit tests of their own |

## Hard rules and tests

Rules 1, 2, 5 and 6 of `docs/design.md` (no release trust, no self-hosted runner,
revert by PR, no push, force or branch delete) are held by construction: the
ports expose no push, branch-delete or artifact-write method, so there is no
code path to test. Rule 3 (the daemon never merges) has a negative test, above.
Rule 4 (approval only for the PR's own head, same repo, allow-listed branch,
opted in) is the `approval::tests::truth_table` row and the watcher and ship
rows named "never approves".

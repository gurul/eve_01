| model | success rate | avg time | avg cost per task (planner) | handoffs | notes |
|---|---|---|---|---|---|
| gpt-6-astra | 18/18 (100%) | 7.2 s | $0.0161 | 0 | finished 18/18 (18 correct); part-done 0; nothing 0; Jev 2.22/task |
| gpt-6-luna | 18/18 (100%) | 6.6 s | $0.0002 | 0 | finished 18/18 (18 correct); part-done 0; nothing 0; Jev 2.28/task |
| gpt-6-sol | 18/18 (100%) | 6.7 s | $0.0036 | 1 | finished 17/18 (17 correct); part-done 1; nothing 0; Jev 2.33/task |

Per task (successes / runs):

| task | gpt-6-astra | gpt-6-luna | gpt-6-sol |
|---|---|---|---|
| contact_form | 2/2 | 2/2 | 2/2 |
| shop_filter_cart | 2/2 | 2/2 | 2/2 |
| search_then_link | 2/2 | 2/2 | 2/2 |
| table_read | 2/2 | 2/2 | 2/2 |
| settings_tab_toggle | 2/2 | 2/2 | 2/2 |
| cookie_banner | 2/2 | 2/2 | 2/2 |
| below_the_fold | 2/2 | 2/2 | 2/2 |
| reservation_form | 2/2 | 2/2 | 2/2 |
| help_read_after_nav | 2/2 | 2/2 | 2/2 |

Pick by the rule (cheapest within 5 points of gpt-6-astra): **gpt-6-luna**

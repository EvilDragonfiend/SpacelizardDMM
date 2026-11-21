#define RETURN_TYPE(X) set SpacemanDMM_return_type = X
#define SKIP_AND_CALL(X) PARAM_LINT_SKIP(X); X
#define PAIR(A, B) A; B

/datum/widget
	proc/run(a, b = 42)
		SHOULD_CALL_REQUIRED_PARAMS(TRUE)
		SHOULD_STATE_PARAM_NAMES(TRUE)
		SHOULD_KEEP_PARAM_ORDER(TRUE)

/datum/widget/New(a)
	SHOULD_CALL_ALL_PARAMS(TRUE)

/proc/get_widget()
	RETURN_TYPE(/datum/widget)

/proc/contracts(list/datum/widget/widgets)
	newlist(/datum/widget) // EXPECT: missing_params
	var/dyn_type = /datum/widget
	newlist(dyn_type) // EXPECT: unverifiable_call
	widgets[1].run(a = 1)
	widgets[1].run(b = 1) // EXPECT: missing_params
	for(var/item in widgets)
		item.run(a = 1)
		item.run(b = 1) // EXPECT: missing_params
	var/reused_item
	for(reused_item in widgets)
		reused_item.run(a = 1)
		reused_item.run(b = 1) // EXPECT: missing_params
	var/w = new /datum/widget(a = 1)
	w.run(a = 1)
	w.run(b = 1) // EXPECT: missing_params
	var/reassigned
	reassigned = new /datum/widget(a = 1)
	reassigned.run(a = 1)
	get_widget().run() // EXPECT: missing_params
	SKIP_AND_CALL(w.run()) // EXPECT: missing_params
	PAIR(PARAM_LINT_SKIP(w.run()), PARAM_LINT_SKIP(w.run()))
	PAIR(w.run(), w.run()) // EXPECT: missing_params*2
	var/list/keyed = list(w.run() = 1) // EXPECT: missing_params
	w.run(a = 1)
	#if 0
	PARAM_LINT_SKIP(w.run(a = 1))
	SHOULD_CALL_ALL_PARAMS(BAD)
	#endif

#ifdef MVP_EXTRA
/proc/case_extra()
	get_widget().run() // EXPECT(extra): missing_params
#endif

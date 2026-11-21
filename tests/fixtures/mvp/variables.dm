#define MUTATE(L, VALUE) L += VALUE
#define NESTED_BYPASS(X) BYPASS_VAR_RULE(X)
#define MUTATE_TWICE(L) L += new /image(); L.Cut()

SET_VAR_RULE(/client/images, ASSIGN_NOT_ALLOWED | LIST_MUTATE_NOT_ALLOWED, allow_in = list(/proc/allowed, /datum/exempt), hint = "Use the helper, or document the bypass.")

/datum/exempt

/datum/protected
	var/list/items
	var/count
	SET_VAR_RULE(items, LIST_MUTATE_NOT_ALLOWED)
	SET_VAR_RULE(count, ASSIGN_NOT_ALLOWED | ALLOW_IN_CONSTRUCTOR)

/datum/protected/child

/datum/protected/New()
	count = 1

/proc/allowed(client/C)
	C.images += new /image()

/datum/exempt/proc/allowed_by_type(client/C)
	C.images.Cut()

/proc/test_blocked(client/C)
	C.images += new /image() // EXPECT: var_assign_forbidden

/proc/test_bypass(client/C)
	C.images += BYPASS_VAR_RULE(new /image())

/proc/test_mutation(client/C, datum/protected/child/sub, dynamic, mob/M, datum/super_ref)
	C.images ||= list() // EXPECT: var_assign_forbidden
	C.images.Cut() // EXPECT: var_list_mutate_forbidden
	C.images[1] = null // EXPECT: var_list_mutate_forbidden
	C.vars["images"] += new /image() // EXPECT: var_assign_forbidden
	BYPASS_VAR_RULE(C.images.Cut())
	C.images += NESTED_BYPASS(new /image())
	MUTATE(C.images, new /image()) // EXPECT: var_assign_forbidden
	M?.client?.images -= new /image() // EXPECT: var_assign_forbidden
	BYPASS_VAR_RULE_BLOCK
		MUTATE_TWICE(C.images)
		C.images |= list()
	MUTATE_TWICE(C.images) // EXPECT: var_assign_forbidden, var_list_mutate_forbidden
	var/list/L = C.images
	L += new /image() // EXPECT: var_alias_escape
	L -= new /image() // EXPECT: var_alias_escape
	L |= list() // EXPECT: var_alias_escape
	L &= list() // EXPECT: var_alias_escape
	L.Add(new /image()) // EXPECT: var_alias_escape
	L.Cut() // EXPECT: var_alias_escape
	L[1] = new /image() // EXPECT: var_alias_escape
	var/list/L2
	L2 = C.images
	L2.Remove(null) // EXPECT: var_alias_escape
	L = list()
	L += BYPASS_VAR_RULE(1) // EXPECT: unused_var_bypass
	dynamic:images += new /image() // EXPECT: unverifiable_var_mutation
	super_ref:count = 1 // EXPECT: unverifiable_var_mutation
	sub.items.Add(1) // EXPECT: var_list_mutate_forbidden
	sub.count++ // EXPECT: var_assign_forbidden
	--sub.count // EXPECT: var_assign_forbidden
	BYPASS_VAR_RULE_BLOCK // EXPECT: unused_var_bypass
		var/unrelated = 1

SET_VAR_RULE(/client/images, ASSIGN_NOT_ALLOWED)

/proc/test_blocked(client/C)
	C.images += new /image() // EXPECT: var_assign_forbidden

/proc/test_bypass(client/C)
	C.images += BYPASS_VAR_RULE(new /image())

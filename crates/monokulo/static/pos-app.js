//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/error.js
var e = class extends Error {
	source;
	constructor(e) {
		let t = Error, n = t.stackTraceLimit;
		n !== void 0 && (t.stackTraceLimit = 0), super(), n !== void 0 && (t.stackTraceLimit = n), this.source = e;
	}
}, t = class extends Error {
	source;
	constructor(e, t) {
		super(t instanceof Error ? t.message : String(t), { cause: t }), this.source = e;
	}
};
function n(e) {
	return e instanceof t ? e.cause : e;
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/constants.js
var r = 1024, i = 2048, a = 4096, o = 1024, s = 4096, c = 16384, l = 1 << 17, u = 1 << 20, d = 1 << 21, f = 1 << 22, p = 1 << 24, m = {}, h = {};
function g(e) {
	return e === h ? void 0 : e;
}
var _ = {}, v = Symbol("refresh"), y = /* @__PURE__ */ new Set();
function b(e) {
	for (; e.cn;) e = e.cn;
	return e;
}
function ee(e, t) {
	if (e = b(e), t = b(t), e === t) return e;
	t.cn = e;
	for (let n of t.ye) e.ye.add(n);
	return t.ye.clear(), e.fn[0].push(...t.fn[0]), e.fn[1].push(...t.fn[1]), t.fn[0].length = 0, t.fn[1].length = 0, e;
}
function te(e) {
	let t = e.o?.Ue;
	if (!t) return;
	let n = b(t);
	if (y.has(n)) return n;
	e.o !== null && (e.o.Ue = void 0);
}
function ne(e) {
	if (sn(e) && e.o?.Ft) {
		let t = H(e).Ft = I(e.o?.Ft);
		if (t.Tt !== !0) return t;
		e.o !== null && (e.o.Ft = null);
	}
	return te(e)?.Ge ?? e.Ge;
}
function re(e, t) {
	let n = b(t), r = e.o?.Ue;
	if (r) {
		let i = b(r);
		if (y.has(i)) {
			i !== n && (!sn(e) || e.T & 8388608) && (n.Ln && b(n.Ln) === i ? (H(e).Ue = t, e.T |= o) : i.Ln && b(i.Ln) === n || ee(n, i));
			return;
		}
	}
	H(e).Ue = t, e.T |= o;
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/scheduler.js
var x = /* @__PURE__ */ new Set(), S = {
	eE: Array(2e3).fill(void 0),
	tE: !1,
	et: 0,
	EE: 0
}, C = {
	eE: Array(2e3).fill(void 0),
	tE: !1,
	et: 0,
	EE: 0
};
function w(e) {
	if (e.ue & 128) return M.We(e);
	e.ue & 16 ? e.ue &= -12 : (Ue(e, C), e.ue &= -4);
}
var T = 0, E = null, D = !1, O = !1, ie = !1, ae = 0, oe = 0, k = /* @__PURE__ */ new Set();
function se(e) {
	let t = e.m;
	return x.size === 0 && y.size === 0 && e.hn.length === 0 && t.rt.length === 0 && t.A.length === 0 && t.dn.size === 0 && k.size === 0;
}
function ce() {
	if (k.size !== 0) for (let e of k) {
		if (e.u !== null) {
			k.delete(e);
			continue;
		}
		e.ve === m && (e.o?.Ce === void 0 || e.o?.Ce === m) && (e.o?.t || (k.delete(e), e.T & 262144 ? Ut(e) : e.o?.Pt?.()));
	}
}
function le() {
	return {
		Pe: T,
		Ot: [],
		oe: /* @__PURE__ */ new Map(),
		rt: [],
		A: [],
		dn: /* @__PURE__ */ new Set(),
		pe: [],
		Sn: {
			mn: [[], []],
			hn: []
		},
		Tt: !1,
		ct: /* @__PURE__ */ new Set(),
		St: null
	};
}
function ue(e, t) {
	t.Tt = e, e.pe.push(...t.pe), e.he ||= t.he;
	for (let n of y) n.Ge === t && (n.Ge = e);
	t.rt.length && (e.rt.push(...t.rt), t.rt.length = 0), t.A.length && (e.A.push(...t.A), t.A.length = 0);
	for (let n of t.dn) e.dn.add(n);
	for (let [n, r] of t.oe) {
		let t = e.oe.get(n);
		t || e.oe.set(n, t = /* @__PURE__ */ new Set());
		for (let e of r) t.add(e);
	}
	for (let n of t.ct) e.ct.add(n);
	t.St && (e.St ??= []).push(...t.St);
}
function A() {
	if (O) {
		he();
		return;
	}
	D || (D = !0, !ae && !F.Kt && queueMicrotask(Me));
}
var j = [];
function de() {
	for (let e of x) j.includes(e) || j.push(e);
	A();
}
var fe = [], pe = Symbol.for("solid-js/root-error-hook");
function me(e) {
	if (O) return;
	O = !0;
	let t = "[REACTIVITY_HALTED]", n = e !== void 0 && globalThis.reportError;
	n || e === void 0 ? console.error(t) : console.error(t, e), n && n(e);
}
function he() {
	ie || (ie = !0, console.error("[REACTIVITY_HALTED]"));
}
var ge = 0, _e = class {
	_parent = null;
	mn = [[], []];
	hn = [];
	An = 0;
	created = T;
	addChild(e) {
		this.hn.push(e), e._parent = this;
	}
	removeChild(e) {
		let t = this.hn.indexOf(e);
		t >= 0 && (this.hn.splice(t, 1), e._parent = null);
	}
	notify(e, t, n, r) {
		return this._parent ? this._parent.notify(e, t, n, r) : !1;
	}
	run(e) {
		if (this.mn[e - 1].length) {
			let t = this.mn[e - 1];
			this.mn[e - 1] = [], Ne(t, e);
		}
		let t = this.hn, n = ++ge;
		for (let r = 0; r < t.length;) {
			let i = t[r];
			if (i.An !== n && (i.An = n, i.run?.(e), t[r] !== i)) {
				r = 0;
				continue;
			}
			r++;
		}
	}
	enqueue(e, t) {
		e && (V ? b(V).fn[e - 1].push(t) : this.mn[e - 1].push(t)), A();
	}
	stashQueues(e) {
		e.mn[0].push(...this.mn[0]), e.mn[1].push(...this.mn[1]), this.mn = [[], []];
		for (let t = 0; t < this.hn.length; t++) {
			let n = this.hn[t], r = e.hn[t];
			r || (r = {
				mn: [[], []],
				hn: []
			}, e.hn[t] = r), n.stashQueues(r);
		}
	}
	restoreQueues(e) {
		this.mn[0].push(...e.mn[0]), this.mn[1].push(...e.mn[1]);
		for (let t = 0; t < e.hn.length; t++) {
			let n = e.hn[t], r = this.hn[t];
			r && r.restoreQueues(n);
		}
	}
}, M = class e extends _e {
	Kt = !1;
	m = le();
	static We;
	static Be;
	static Et;
	static Cn = null;
	static p = null;
	static G = null;
	static M = null;
	static N = null;
	static wt = null;
	static Yt = null;
	static me = null;
	static Oe = null;
	static Me = null;
	static En = null;
	static zt = null;
	static Jt = null;
	static nn = null;
	static Nt = null;
	static k = null;
	static pn = null;
	static vn = null;
	static ln = null;
	static On = null;
	static Nn = null;
	static Rn = null;
	static _n = null;
	static In = null;
	static tn = null;
	static $t = null;
	static Xt = null;
	static Bt = null;
	static st = null;
	static _t = null;
	static Zt = null;
	static ft = null;
	static we = null;
	static Tn = null;
	static en = null;
	static Ve = null;
	static jt = !1;
	static un = null;
	static Dn = null;
	flush() {
		if (!this.Kt) {
			if (E === null && S.EE < S.et && this.mn[0].length === 0 && this.mn[1].length === 0 && this.hn.length === 0 && !j.length && !fe.length && se(this)) {
				this.Kt = !0;
				try {
					dn(), dt(), De();
				} finally {
					this.Kt = !1;
				}
				T++, D = S.EE >= S.et || this.mn[0].length !== 0 || this.mn[1].length !== 0 || this.m.Ot.length !== 0;
				return;
			}
			this.Kt = !0, dn();
			try {
				for (; fe.length;) this.initTransition(fe.pop());
				if (dt(), Ke(S, e.We), E) {
					if (e.Tn?.(E) && Ke(S, e.We), !Ie(E)) {
						let t = E;
						Ee.length = 0, Ke(C, this.m === t ? w : e.We), this.m === t && (je = this.m = le()), y.size && (e._n(1), e._n(2)), this.stashQueues(t.Sn), T++, D = S.EE >= S.et || this.m.Ot.length > 0, Ae(t.Ot), E = null, Oe(null, !0);
						return;
					}
					let t = E, n = this.m;
					if (n !== t && n.Ot.push(...t.Ot), this.restoreQueues(t.Sn), x.delete(t), E = null, Ae(n.Ot), Oe(t), n === t) {
						let e = le();
						e.Ot = n.Ot, e.rt = n.rt, e.A = n.A, e.dn = n.dn, je = this.m = e;
					}
				} else se(this) ? (De(), S.EE >= S.et && (Ke(S, e.We), De())) : (x.size && Ke(C, e.We), Oe());
				T++, D = S.EE >= S.et || E !== null, y.size && e._n(1), this.run(1), y.size && e._n(2), this.run(2);
			} finally {
				for (; !D && !E && j.length;) this.initTransition(j.pop());
				this.Kt = !1;
			}
		}
	}
	notify(t, n, r, i) {
		if (n & 1) {
			if (r & 1) {
				let n = i ?? t.o?._;
				if (n?.l) return !0;
				if (n && (!E && !t.Ge && je.Ot.length && this.initTransition(), E)) {
					let r = n.source, i = E.oe.get(r);
					i || E.oe.set(r, i = /* @__PURE__ */ new Set());
					let a = i.size;
					i.add(t), i.size !== a && (A(), e.vn?.(E));
				}
			}
			return !0;
		}
		return !1;
	}
	initTransition(e) {
		if (e && (e = I(e), e.Tt === !0 || e === E) || !e && E && E.Pe === T) return;
		if (!E) E = e ?? le();
		else if (e) {
			let t = E;
			ue(e, t), this.restoreQueues(t.Sn), x.delete(t), E = e;
		}
		x.add(E), E.Pe = T;
		let t = this.m;
		if (t !== E) {
			let e = this.Kt ? 0 : p;
			for (let n = 0; n < t.Ot.length; n++) {
				let r = t.Ot[n];
				if (r.Ge === null && r.ve !== m && (!r.ce || r.ue & 1024 && !(r.S & 4)) && r.Fe && r.Fe(r.Qe, r.ve)) {
					r.ve = m, Ce(r);
					continue;
				}
				r.Ge = E, r.T |= e, E.Ot.push(r);
			}
			for (let e = 0; e < t.rt.length; e++) {
				let n = t.rt[e];
				n.Ge = E, E.rt.push(n);
			}
			t.A.length && E.A.push(...t.A);
			for (let e of t.dn) E.dn.add(e);
			if (t.ct.size) {
				for (let e of t.ct) E.ct.add(e);
				t.ct.clear();
			}
			je = this.m = E;
		}
		for (let e of y) e.Ge ||= E;
		A();
	}
};
function N(e) {
	je.Ot.push(e), F.Kt || tn();
}
var ve = !1, ye = 0;
function P() {
	ye++;
}
var be = 0;
function xe(e) {
	let t = be;
	return be = e, t;
}
function Se(e, t = !1) {
	e.ht = ye;
	let n = e.T, r = (n & 1024 ? e.o?.Ue : void 0) || V, o = !!(n & 512) && e.o?.nt !== void 0, s = ve;
	for (let n = e.u; n !== null; n = n.Ne) {
		let e = n._e;
		if (s && (e.ue &= ~i), e.ue & 4 && n.qe === e.Ze && n !== e.ot && (e.ue |= a), o && e.T & 8) {
			e.ue |= 256;
			continue;
		}
		t && r ? (e.ue |= 128, re(e, r)) : t && (e.ue |= 128, e.o && (e.o.Ue = void 0)), R(e);
	}
}
function Ce(e) {
	let t = e;
	if (!t.ce) {
		e.ve !== m && (e.Qe = e.ve, e.ve = m), e.T & 256 && M.En(e);
		return;
	}
	e.ve !== m && (e.Qe = e.ve, e.ve = m, t.S &= -5, e.Le && e.Le !== 3 && (e.Ye = !0), e.o && (e.o.be = !1)), t.ge = !1, t.ue &= ~r, t.o?._ ?? st(t), t.T &= ~u, t.S & 1 ? e.T |= d : t.S &= -5, t.o != null && (t.o.lt !== null || t.o.it !== null) && M.Be(t, !1, !0), e.T & 256 && M.En(e);
}
var we = null, Te = [], Ee = [];
function De() {
	for (; Ee.length;) st(Ee.pop());
	let e = je.Ot;
	for (let t = 0; t < e.length; t++) {
		let n = e[t];
		Ce(n), n.Ge = null, n.T & 131072 && (n.T &= ~l, Te.push(n));
	}
	e.length = 0, we?.();
}
function Oe(e = null, t = !1) {
	let n = je, r = !t;
	r && De(), !t && F.hn.length && ke(F);
	let i = e?.St, a = r && (e ?? n).rt.length !== 0;
	if (i && !a) for (let e of i) e.ue & 64 || R(e);
	let o = S.EE >= S.et;
	if (o && Ke(S, M.We), r) {
		if (je !== n) {
			if (e === null || e === n) return;
		} else o && De();
		let t = e ?? n;
		if (t.rt.length && M.On(t.rt), i && a) {
			for (let e of i) e.ue & 64 || R(e);
			A();
		}
		if (t.ct.size) {
			for (let e of t.ct) e.ue & 64 || R(e);
			t.ct.clear(), A();
		}
		if (t.A.length && (M.G(t.A), F.hn.length && ke(F)), t.dn.size && M.Cn(t.dn, e), Te.length !== 0) {
			for (; Te.length;) Se(Te.pop());
			S.EE >= S.et && (Ke(S, M.We), De());
		}
		ce(), y.size && M.Rn(e);
	}
}
function ke(e) {
	for (let t of e.hn) t.fe?.(), ke(t);
}
function Ae(e) {
	for (let t = 0; t < e.length; t++) e[t].Ge = E, e[t].T &= ~p;
}
var F = new M(), je = F.m;
function Me(e) {
	if (oe > 0) return e ? e() : void 0;
	if (e) {
		ae++;
		try {
			return e();
		} finally {
			try {
				Me();
			} finally {
				ae--;
			}
		}
	}
	if (!F.Kt && !O) {
		for (; D || E;) F.flush();
		be = 0;
	}
}
function Ne(e, t) {
	for (let n = 0; n < e.length; n++) e[n](t);
}
function Pe(t, n, r) {
	let i = t.ue;
	if (i & 64) return !1;
	if (i & 32) {
		let e = t;
		for (; e && e.ue & 32;) e = e._parent;
		let n = e && (e.Ge || (e.T & 1048576 ? E : null));
		if (!n || (n = I(n)).Tt === !0 || n === r) return !1;
	}
	for (let e = t.C; e; e = e._parent) if (e.ee & 1 && !e.L) return !1;
	if (t.o?.ae?.has(n)) return !0;
	let a = t.ot;
	for (let e = a === null ? null : t.Se; e; e = e === a ? null : e.de) {
		let t = e.Ee;
		for (; t;) {
			if (t === n || t.Te === n || t.o?.ae?.has(n)) return !0;
			t = t.o?.Ht;
		}
	}
	return !!(t.S & 1 && t.o?._ instanceof e && t.o?._.source === n);
}
function Fe(e, t, n) {
	let r = e.oe.get(t), i = !1;
	for (let e of r ?? []) {
		if (Pe(e, t, n)) return !0;
		n && e.ue & 32 ? i = !0 : r.delete(e);
	}
	return i || e.oe.delete(t), !1;
}
function Ie(e) {
	if (e.Tt) return !0;
	if (e.pe.length) return !1;
	let t = !0;
	for (let n of e.oe.keys()) if (Fe(e, n, e) && n.o?.ae?.size) {
		t = !1;
		break;
	}
	return t && M.Nn?.(e) && (t = !1), t && (e.Tt = !0), t;
}
function I(e) {
	for (; e.Tt && typeof e.Tt == "object";) e = e.Tt;
	return e;
}
function Le(e) {
	for (let t of x) if (Fe(t, e)) return t;
	return null;
}
function Re(e) {
	for (let t of x) Fe(t, e) && F.initTransition(t);
}
function ze(e, t) {
	let n = E;
	try {
		return E = I(e), t();
	} finally {
		E = n;
	}
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/heap.js
function L(e) {
	return e.ue & 32 ? C : S;
}
function R(e) {
	let t = L(e);
	t.et > e.tt && (t.et = e.tt), Ve(e, t);
}
function Be(e, t) {
	let n = (e._parent?.xt ? e._parent.Qt?.tt : e._parent?.tt) ?? -1;
	n >= e.tt && (e.tt = n + 1);
	let r = e.tt, i = t.eE[r];
	if (i === void 0) t.eE[r] = e;
	else {
		let t = i.Rt;
		t.At = e, e.Rt = t, i.Rt = e;
	}
	r > t.EE && (t.EE = r);
}
function Ve(e, t) {
	let n = e.ue;
	n & 1036 || (n & 1 ? e.ue = n & -4 | 10 : (e.ue = n | 8, t.tE && Ge(e)), n & 16 || Be(e, t));
}
function He(e, t) {
	let n = e.ue;
	n & 1052 || (e.ue = n | 16, Be(e, t));
}
function Ue(e, t) {
	let n = e.ue;
	if (!(n & 24)) return;
	e.ue = n & -25;
	let r = e.tt;
	if (e.Rt === e) t.eE[r] = void 0;
	else {
		let n = e.At, i = t.eE[r], a = n ?? i;
		e === i ? t.eE[r] = n : e.Rt.At = n, a.Rt = e.Rt;
	}
	e.Rt = e, e.At = void 0;
}
function We(e) {
	if (!e.tE) {
		e.tE = !0;
		for (let t = 0; t <= e.EE; t++) for (let n = e.eE[t]; n !== void 0; n = n.At) n.ue & 8 && Ge(n);
	}
}
function Ge(e, t = 2) {
	let n = e.ue;
	if (!((n & 3) >= t)) {
		e.ue = n & -4 | t;
		for (let t = e.u; t !== null; t = t.Ne) Ge(t._e, 1);
		if (e.T & 4096) for (let t = e.o.i; t !== null; t = t.De) for (let e = t.u; e !== null; e = e.Ne) Ge(e._e, 1);
	}
}
function Ke(e, t) {
	for (e.tE = !1, e.et = 0; e.et <= e.EE; e.et++) {
		let n = e.eE[e.et];
		for (; n !== void 0;) n.ue & 8 ? t(n) : qe(n, e), n = e.eE[e.et];
	}
	e.EE = 0;
}
function qe(e, t) {
	Ue(e, t);
	let n = e.tt;
	for (let t = e.Se; t; t = t.de) {
		let e = t.Ee, r = e.Te || e;
		r.ce && r.tt >= n && (n = r.tt + 1);
	}
	if (e.tt !== n) {
		e.tt = n;
		for (let t = e.u; t !== null; t = t.Ne) He(t._e, L(t._e));
	}
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/owner.js
function Je(e) {
	let t = e.Xe;
	for (; t;) {
		let e = t.ue;
		t.ue = e | 32, e & 24 && (Ue(t, e & 32 ? C : S), e & 8 ? Ve(t, C) : He(t, C)), Je(t), t = t.$e;
	}
}
function Ye(e, t = !1, n) {
	let r = e.ue;
	if (r & 64) return;
	if (t) {
		e.ue = r | 64;
		let t = e;
		(t.o?.je || t.o?.xe) && M.En(t), t.T & 2048 && t.o.bt.forEach(M.En);
		let n = t.Ge;
		n && t.S & 1 && !j.includes(n) && (j.push(n), A());
	}
	t && e.ce && e.o !== null && (e.o.Re = null);
	let i = n ? e.o?.lt ?? null : e.Xe;
	for (; i;) {
		let e = i.$e, t = i;
		t.T &= -33, Ue(t, L(t)), ct(t), Ye(i, !0), i = e;
	}
	if (n ? e.o !== null && (e.o.lt = null) : (e.Xe = null, e.ut = 0), t && !n && !(r & 32) && e._parent !== null && !(e._parent.ue & 64)) {
		let t = e.Dt, n = e.$e;
		t === null ? e._parent.Xe = n : t.$e = n, n !== null && (n.Dt = t), e.Dt = null;
	}
	if (Xe(e, n), t && e.yt) {
		let t = e.yt;
		e.yt = void 0, t();
	}
}
function Xe(e, t) {
	let n = t ? e.o?.it : e.ke;
	if (n) {
		if (Array.isArray(n)) for (let e = 0; e < n.length; e++) {
			let t = n[e];
			t.call(t);
		}
		else n.call(n);
		t ? e.o !== null && (e.o.it = null) : e.ke = null;
	}
}
function Ze(e, t) {
	let n = e;
	for (; n.T & 4 && n._parent;) n = n._parent;
	if (n.id != null) return et(n.id, t ? n.ut++ : n.ut);
	throw Error("");
}
function Qe(e) {
	return Ze(e, !0);
}
function $e(e, t, n) {
	return e?.id ?? (t ? n?.id : n?.id == null ? void 0 : Qe(n));
}
function et(e, t) {
	let n = t.toString(36), r = n.length - 1;
	return e + (r ? String.fromCharCode(64 + r) : "") + n;
}
function tt() {
	return B;
}
function nt(e) {
	return B && (B.ke ? Array.isArray(B.ke) ? B.ke.push(e) : B.ke = [B.ke, e] : B.ke = e), e;
}
function rt(e = !0) {
	Ye(this, e);
}
function it(e) {
	let t = B, n = e?.transparent ?? !1, r = {
		id: $e(e, n, t),
		T: n ? 4 : 0,
		xt: !0,
		Qt: t?.xt ? t.Qt : t,
		Xe: null,
		$e: null,
		Dt: null,
		ke: null,
		C: t?.C ?? F,
		ze: t?.ze || _,
		ut: 0,
		o: null,
		_parent: t,
		dispose: rt
	};
	if (t) {
		let e = t.Xe;
		e === null ? t.Xe = r : (r.$e = e, e.Dt = r, t.Xe = r);
	}
	return r;
}
function at(e, t) {
	let n = it(t);
	return yn(n, () => e(() => n.dispose()));
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/graph.js
function ot(e) {
	let t = e.Ee, n = e.de, r = e.Ne, i = e.rn;
	if (r === null ? t.Gt = i : r.rn = i, i !== null) i.Ne = r;
	else if (t.u = r, r === null) {
		t.T & 262144 ? Ut(t) : t.o?.Pt?.();
		let e = t;
		e.ce && e.T & 32 && !(e.ue & 32) && !(e.S & 1) && lt(e);
	}
	return n;
}
function st(e) {
	let t = e.ot, n = t === null ? e.Se : t.de;
	if (n !== null) {
		do
			n = ot(n);
		while (n !== null);
		t === null ? e.Se = null : t.de = null;
	}
}
function ct(e) {
	let t = e.Se;
	if (t) {
		do
			t = ot(t);
		while (t !== null);
		e.Se = null, e.ot = null;
	}
}
function lt(e) {
	Ue(e, L(e)), ct(e), Ye(e, !0);
}
var ut = /* @__PURE__ */ new Set();
function dt() {
	if (ut.size !== 0) {
		for (let e of ut) !e.u && e.T & 32 && !(e.S & 1) && !(e.ue & 96) && lt(e);
		ut.clear();
	}
}
function ft(e, t, n = !1) {
	let r = t.ot;
	if (r !== null && r.Ee === e) {
		r.He &&= n;
		return;
	}
	let i = null, a = t.ue & 4;
	if (a && (i = r === null ? t.Se : r.de, i !== null && i.Ee === e)) {
		i.qe = t.Ze, t.ot = i, i.He = n;
		return;
	}
	let o = e.Gt;
	if (o !== null && o._e === t && (!a || o.qe === t.Ze)) {
		a ? o.He &&= n : o.He = n;
		return;
	}
	let s = t.ot = e.Gt = {
		Ee: e,
		_e: t,
		de: i,
		rn: o,
		Ne: null,
		qe: t.Ze,
		He: n
	};
	r === null ? t.Se = s : r.de = s, o === null ? e.u = s : o.Ne = s, P();
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/async.js
function pt(e, t) {
	return !e.o?.ae?.has(t) && ((H(e).ae ??= /* @__PURE__ */ new Set()).add(t), !0);
}
function mt(e, t) {
	let n = e.o?.ae;
	return n?.delete(t) ? (n.size || (e.o.ae = void 0), !0) : !1;
}
function ht(e) {
	e.o !== null && (e.o.ae = void 0);
}
function gt(e, t) {
	for (let n = e.Se; n; n = n.de) {
		let e = n.Ee.Te || n.Ee;
		if (e === t || e.o?.ae?.has(t)) return !0;
	}
	return !1;
}
function _t(e, t) {
	H(e).Ie = !0, t.source && pt(e, t.source), e.S & 2 || vt(e, t.source, t);
}
function vt(t, n, r) {
	if (!n) {
		t.o !== null && (t.o._ = null);
		return;
	}
	if (r instanceof e && r.source === n) {
		H(t)._ = r;
		return;
	}
	let i = t.o?._;
	(!(i instanceof e) || i.source !== n) && (H(t)._ = new e(n));
}
function yt(e, t) {
	for (let n = e.u; n !== null; n = n.Ne) t(n._e, n);
	for (let n = e.o?.i ?? null; n !== null; n = n.De) for (let e = n.u; e !== null; e = e.Ne) t(e._e, e);
}
function bt(e) {
	e.ce && e.T & 32 && !e.u && !(e.ue & 32) && !(e.S & 1) && lt(e);
}
function xt(e) {
	let t, n = /* @__PURE__ */ new Set(), r = (e) => {
		n.has(e) || (n.add(e), !e.u && e.T & 32 && (t ??= []).push(e), yt(e, r));
	};
	if (yt(e, r), t) for (let e of t) bt(e);
}
function St(e, t) {
	let n = !1, r = /* @__PURE__ */ new Set(), i = (e) => {
		r.has(e) || (r.add(e), e.o?._ === t && (R(e), n = !0), yt(e, i));
	};
	yt(e, i), n && A();
}
function Ct(e, t = e) {
	mt(e, t);
	let n = !1, r, i = /* @__PURE__ */ new Set(), a = M.Oe, o = (s) => {
		if (i.has(s) || t !== e && gt(s, t) || !mt(s, t)) return;
		i.add(s), s.Pe = T;
		let c = s.o?.ae?.values().next().value, l = s.S & 2;
		c ? (l || vt(s, c), a?.(s)) : (s.S &= -2, l || vt(s), a?.(s), s.o?.Ie && (R(s), n = !0), s.o !== null && (s.o.Ie = !1), !s.u && s.T & 32 && (r ??= []).push(s)), yt(s, o);
	};
	if (yt(e, o), r) for (let e of r) bt(e);
	n && A();
}
function wt(e) {
	return typeof e == "object" && !!e && typeof e.then == "function";
}
function Tt(e) {
	let t = e.o?.Ae;
	t != null && (e.o.Ae = null, t());
}
function Et(t, n, r) {
	let i = !1, a = !1;
	if (typeof n == "object" && n && Kt(() => {
		i = n[Symbol.asyncIterator], a = !i && wt(n);
	}), !a && !i) return t.o !== null && (t.o.Re = null), t.ge = !1, n;
	H(t).Re = n, t.o.ae = void 0;
	let o = be, s, c = () => {
		let e = ne(t);
		if (t.o?.Ue && (e = Le(t) ?? e), e && t.S & 4 && !I(e).oe.has(t)) {
			t.Ge = null;
			return;
		}
		F.initTransition(e), Re(t);
	}, l = (r) => {
		if (t.o?.Re !== n) return;
		let i = r instanceof e;
		if (i && t.ge) {
			t.o !== null && (t.o.Re = null), _t(t, r), t.Pe = T;
			return;
		}
		c(), kt(t, i ? 1 : 2, r), i && Ct(t), t.Pe = T, i || xt(t);
	}, u = (e, i) => {
		if (t.o?.Re !== n || t.ue & 130) return;
		xe(o), c();
		let a = !!(t.S & 4), s = t.o?.be;
		Ot(t), s && (t.o.be = !0);
		let u = te(t);
		if (u && u.ye.delete(t), r) {
			try {
				r(e);
			} catch (e) {
				l(e);
				return;
			}
			a && Ot(t, !0);
		} else if (t.o?.Ce !== void 0 && !(u && t.T & 8388608)) t.ve === m && N(t), t.ve = e, M.me?.(t, e), sn(t) ? M.we(t, e) : Se(t), t.Pe = T;
		else if (u) {
			let n = t.Le, r = sn(t) ? g(t.o.Ce) : t.Qe, i = t.Fe;
			try {
				(!n && a || !i || !i(e, r)) && (n ? t.Qe = e : M.Ve(t, e, u), t.Pe = T, M.me?.(t, e), Se(t, !0));
			} catch (e) {
				kt(t, 2, e);
			}
		} else try {
			gn(t, () => e);
		} catch (e) {
			kt(t, 2, e);
		}
		t.ve === m && (t.ge = !1, s && (t.o.be = !1), st(t)), Ct(t), A(), Me(), i?.();
	}, d = () => t.T & 32 && !t.u && !(t.S & 1) ? (lt(t), !0) : !1, f = (e, r) => {
		let i = e[Symbol.asyncIterator](), a = !1, o = !1, c = !r, f = () => {
			if (!o) {
				o = !0;
				try {
					let e = i.return?.();
					wt(e) && e.then(void 0, () => {});
				} catch {}
			}
		};
		r ? r(f) : nt(f), H(t).Ae = f;
		let p = () => {
			d() || m();
		}, m = () => {
			let e, r, f = !1, h = !1, g = !0, _ = i.next();
			if ((wt(_) ? _ : { then: (e) => void e(_) }).then((r) => {
				if (g && c) e = r, f = !0, r.done && (o = !0);
				else if (t.o?.Re !== n) return;
				else r.done ? (o = !0, a ? (A(), Me()) : u(void 0), d()) : (a = !0, u(r.value, p));
			}, (e) => {
				g && c ? (r = e, h = !0) : t.o?.Re === n && (o = !0, l(e), d());
			}), g = !1, h) {
				if (o = !0, l(r), c) throw r;
				return !0;
			}
			return f && !e.done ? (s = e.value, a = !0, m()) : f && e.done;
		}, h = m();
		return c = !1, a || h;
	}, p = null, h = (e, t) => {
		let n = !1;
		if (typeof e == "object" && e && Kt(() => {
			n = e[Symbol.asyncIterator];
		}), !n) return !1;
		let r = f(e, t);
		return t || (p = r), !0;
	};
	if (a) {
		let r = !1, i = !1, a, o = !0, c = (e) => {
			t.ke ? Array.isArray(t.ke) ? t.ke.push(e) : t.ke = [t.ke, e] : t.ke = e;
		};
		if (n.then((e) => {
			o ? (s = e, r = !0) : t.o?.Re === n && !(t.ue & 64) && h(e, c) || (u(e), d());
		}, (e) => {
			o ? (a = e, i = !0) : (l(e), d());
		}), o = !1, i) throw l(a), a;
		if (r) h(s) || (t.ge = !1);
		else {
			if (t.ge) return t.Qe;
			throw F.initTransition(ne(t)), new e(B);
		}
	}
	if (i && h(n), p !== null) {
		if (!p) {
			if (t.ge) return t.Qe;
			throw F.initTransition(ne(t)), new e(B);
		}
		t.ge = !1;
	}
	return s;
}
function Dt(e, t = !1) {
	e.o?.ae && ht(e), e.o?.Ie && e.o !== null && (e.o.Ie = !1), e.o !== null && (e.o.be = !1), e.S = t ? 0 : e.S & 4, e.o?._ && vt(e), (e.o?.je || e.o?.xe) && M.Oe(e), e.o?.i && e.T & 2048 && M.Me !== null && M.Me(e);
	let n = zt(e);
	n && n.call(e);
}
function Ot(e, t = !1) {
	let n = e.o?.ae;
	n && (n.delete(e), n.size) ? (e.o.Ie = !1, t && (e.S = 1), vt(e, n.values().next().value)) : Dt(e, t);
}
function kt(n, r, i, a, o) {
	r === 2 && !(i instanceof t) && !(i instanceof e) && (i = new t(n, i));
	let s = r === 1 && i instanceof e ? i.source : void 0, c = s === n, l = r === 1 && n.o?.Ce !== void 0 && !(n.T & 8388608) && !c, u = l && sn(n);
	a || (o && re(n, o), r === 1 && s ? (pt(n, s), n.S & 1 || (n.T &= ~d), n.S = 1 | n.S & 4, vt(n, s, i)) : (ht(n), n.S = r | (r === 2 ? 0 : n.S & 4), H(n)._ = i), M.Oe?.(n), n.o?.i && n.T & 2048 && M.Me !== null && M.Me(n));
	let f = a || u, p = a || l ? void 0 : o, h = zt(n);
	if (h) {
		if (a && r === 1) return;
		f ? h.call(n, r, i) : h.call(n);
		return;
	}
	yt(n, (t, n) => {
		if (t.Pe = T, r === 1 && n.qe !== t.Ze) {
			R(t), A();
			return;
		}
		if (r === 1 && s && !t.o?.ae?.has(s) || r !== 1 && (t.o?._ !== i || t.o?.ae)) {
			if (n.He && r !== 1 && !(i instanceof e)) {
				R(t), A();
				return;
			}
			f || (t.Ge ? s && !t.Le && (t.S & 1 || t.ve !== m) && F.initTransition(t.Ge) : N(t)), kt(t, r, i, f, p);
		}
	});
}
M.We = (e) => {
	e.Le === 3 ? (Ue(e, L(e)), e.Ye = !0, e.C.enqueue(2, e.Ke)) : Nt(e);
}, M.Be = Ye;
var z = !1, At = !1, jt = !1, Mt = !1, B = null, V = null;
function Nt(t, n = !1) {
	P();
	let r = t.Le;
	if (!n) {
		if (t.Ge && !r && E !== t.Ge && F.initTransition(t.Ge), Ue(t, L(t)), t.o !== null && (t.o.Re = null, Tt(t)), r === 3 || t.T & 1048576) Ye(t);
		else if (t.Xe !== null || t.ke !== null) {
			Je(t);
			let e = H(t);
			e.it = t.ke, e.lt = t.Xe, t.ke = null, t.Xe = null, t.ut = 0;
		}
	}
	let o = !!(t.ue & 128), s = !!(t.T & 8388736) && t.o?.Ce !== m && t.o?.Ce !== void 0, c = !!(t.S & 4), l = t.S & 2 ? t.o?._ : void 0, d = !!(t.S & 1), f = d ? t.o?.ae : void 0, p = t.o?.ae?.has(t), h = (t.ue & i) !== 0, _ = t.ge, v = Zt;
	Zt = null;
	let y = B;
	B = t, t.ot = null, t.Ze++, t.ue = 4, t.Pe = T;
	let b = t.ve === m ? t.Qe : t.ve, ee = t.tt, te = !1, ne = z, re = V;
	z = !0;
	let x = Mt;
	if (Mt = !1, r || (V = null), o) {
		let e = M.st(t, !0);
		e ? V = e : e === !1 && (o = !1);
	} else if (t.T & 8388608) {
		let e = M.st(t, !0);
		e && (o = !0, V = e);
	} else if (E && !n && E.rt.length) {
		let e = M.st(t, !1);
		e && (o = !0, V = e);
	}
	let S = r && r !== 2, C = At;
	S && (At = !0), r && E !== null && E.ct.size && E.ct.delete(t);
	try {
		if (t.T & 64) b = t.ce(b), t.o !== null && (t.o.Re = null), t.ge = !1;
		else {
			let e = t.o?.Re, n = t.ce(b), r = typeof n == "object" && !!n, i = t.o?.Re !== e;
			b = i || !r ? n : Et(t, n), !i && !r && (t.o !== null && (t.o.Re = null), t.ge = !1);
		}
		(t.S !== 0 || t.o !== null) && Dt(t, n && Zt === null), t.T & 1024 && t.o?.Ue && M.ft(t);
	} catch (n) {
		let r = n instanceof e;
		if (r && t.ge) _t(t, n);
		else {
			r && V && M._t(t);
			let e = !1;
			if (r && (H(t).Ie = !0, M.Nt !== null && (e = M.Nt(t, h))), kt(t, r ? 1 : 2, n, void 0, r ? t.o?.Ue : void 0), r && p && !t.o?.Re && Ct(t), r && f) for (let e of f) e !== t && !t.o?.ae?.has(e) && Ct(t, e);
			e && M.k(t);
		}
	} finally {
		z = ne, Mt = x, S && (At = C), te = (t.ue & a) !== 0, t.ue = 0 | (n ? t.ue & 256 : 0), B = y;
	}
	let w = Zt;
	if (Zt = v, !t.o?._) {
		let e = s ? g(t.o?.Ce) : o || t.ve === m ? t.Qe : t.ve, i = !1;
		try {
			i = !r && c || !t.Fe || !t.Fe(e, b);
		} catch (e) {
			kt(t, 2, e);
		}
		if (r && i && (t.Ye = !t.o?._, !n)) {
			t.C.enqueue(r, t.dt ??= M.Et.bind(null, t));
			let e = t.It;
			e !== E && (t.It = E, e !== null && (e = I(e)) !== E && !e.Tt && ((e.St ??= []).push(t), E !== null && (E.St ??= []).push(t)));
		}
		if (!t.o?._) {
			if (i) {
				let e = s ? t.o?.Ce : void 0;
				n && w === null || r && w === null && (E !== t.Ge || E === null || t.T & 32768) || o ? (o && !r && V !== null ? M.Ve(t, b, V) : t.Qe = b, o && (t.ve = m)) : (t.ve = b, w !== null && (t.Ge = w, w.Ot.push(t), r && w.ct.add(t)), _ && (t.ge = !0), t.T & 256 && M.me !== null && M.me(t, b)), t.u !== null && (!s || o || t.o?.Ce !== e) ? Se(t, o || s) : s && !o && t.o.Ct !== T && M.we(t, b);
			} else if (s) t.ve === m && N(t), t.ve = b, _ && (t.ge = !0), M.we(t, b);
			else if (t.tt != ee) for (let e = t.u; e !== null; e = e.Ne) He(e._e, L(e._e));
		}
		if (!i && !t.o?._ && (l !== void 0 && St(t, l), f)) for (let e of f) e !== t && Ct(t, e);
		p && !(t.S & 5) && Ct(t);
	}
	let D = t.ot;
	r && (d && !(t.S & 1) || (D === null ? t.Se !== null : D.de !== null)) && de(), !t.o?._ && t.ve === m && !(r && t.Ye) && (n || o || r === 3 ? st(t) : (t.ot?.de ?? t.Se) && Ee.push(t)), V = re;
	let O = (t.ve !== m || t.o !== null && (t.o.lt !== null || t.o.it !== null) || !!(t.S & 5)) && (!n || w !== null || !!(t.S & 1));
	if (O && (!t.Ge || s) ? N(t) : O && E === null && !(t.S & 5) && (O = !1, Ye(t, !1, !0)), O ? t.T |= u : t.T &= ~u, t.Ge && r && E !== t.Ge && w === null) {
		let e = t.It;
		ze(t.Ge, () => Nt(t)), t.It = e;
	}
	te && (R(t), A());
}
function Pt(e) {
	if (!(e.ue & 68)) {
		if (e.ue & 1) for (let t = e.Se; t; t = t.de) {
			let n = t.Ee, r = n.Te || n;
			if (r.ce && Pt(r), e.ue & 2) break;
		}
		(e.ue & 130 || e.o?._ && e.Pe < T && !e.o?.Re) && Nt(e), e.ue &= 280;
	}
}
function Ft(e, t) {
	let n = t?.transparent ?? !1, r = typeof t == "object" && !!t && "loadingValue" in t, i = {
		id: $e(t, n, B),
		T: (n ? 4 : 0) | !!t?.ownedWrite | (!B || t?.lazy ? 32 : 0) | (t?.sync ? 64 : 0) | (t?.Z ? 2 : 0) | 0,
		Fe: t?.equals ?? Gt,
		ke: null,
		C: B?.C ?? F,
		ze: B?.ze ?? _,
		ut: 0,
		ce: e,
		Qe: r ? t.loadingValue : void 0,
		tt: 0,
		At: void 0,
		Rt: null,
		Se: null,
		ot: null,
		Ze: 0,
		u: null,
		Gt: null,
		_parent: B,
		$e: null,
		Dt: null,
		Xe: null,
		ue: t?.lazy ? 512 : 0,
		S: r ? 0 : 4,
		Pe: T,
		ve: m,
		Ge: null,
		ht: -1,
		ge: r,
		o: null
	};
	return t?.unobserved && (H(i).Pt = t.unobserved), Vt(i, t), i;
}
function H(e) {
	return e.o ??= {
		Ce: void 0,
		Ft: void 0,
		Ct: 0,
		gt: m,
		vt: 0,
		Ue: void 0,
		je: void 0,
		xe: void 0,
		Ht: void 0,
		t: 0,
		Re: null,
		Ae: null,
		_: void 0,
		Ie: void 0,
		ae: void 0,
		h: void 0,
		be: !1,
		i: null,
		Pt: void 0,
		nt: void 0,
		it: null,
		lt: null,
		bt: void 0
	};
}
function It(e, t, n, r, i) {
	let a = i?.transparent ?? !1, o = {
		id: $e(i, a, B),
		T: (a ? 4 : 0) | !!i?.ownedWrite | (i?.sync ? 64 : 0) | (i?.kt ?? 0) | 0,
		Fe: !1,
		ke: null,
		C: B?.C ?? F,
		ze: B?.ze ?? _,
		ut: 0,
		ce: e,
		Qe: void 0,
		tt: 0,
		At: void 0,
		Rt: null,
		Se: null,
		ot: null,
		Ze: 0,
		u: null,
		Gt: null,
		_parent: B,
		$e: null,
		Dt: null,
		Xe: null,
		ue: 512,
		S: 4,
		Pe: T,
		ve: m,
		Ge: null,
		ht: -1,
		ge: !1,
		Ye: !1,
		Ut: void 0,
		Lt: t,
		Vt: n,
		yt: void 0,
		Le: r,
		It: null,
		o: null
	};
	return i?.unobserved && (H(o).Pt = i.unobserved), Vt(o, Bt), o;
}
var Lt = null;
function Rt(e) {
	Lt = e;
}
function zt(e) {
	let t = e.o?.h;
	return t === void 0 ? e.Le ? Lt ?? void 0 : void 0 : t;
}
var Bt = { lazy: !0 };
function Vt(e, t) {
	e.Rt = e;
	let n = B?.xt ? B.Qt : B;
	if (B) {
		let t = B.Xe;
		t === null ? B.Xe = e : (e.$e = t, t.Dt = e, B.Xe = e);
	}
	n && (e.tt = n.tt + 1), M.wt !== null && M.wt(e), !t?.lazy && Nt(e, !0);
}
function Ht(e, t, n = null) {
	let r = {
		Fe: t?.equals ?? Gt,
		T: +!!t?.ownedWrite | (t?.Z ? 2 : 0),
		Qe: e,
		u: null,
		Gt: null,
		Pe: T,
		Te: n,
		De: n?.o?.i || null,
		Mt: null,
		ve: m,
		Ge: null,
		ht: -1,
		o: null
	};
	return t?.unobserved && (H(r).Pt = t.unobserved), n && Wt(n, r), r;
}
var Ut;
function Wt(e, t) {
	let n = t.De;
	n !== null && (n.Mt = t), H(e).i = t, e.T |= s;
}
function Gt(e, t) {
	return e === t;
}
function Kt(e, t) {
	if (M.Yt === null && !z) return e();
	let n = z;
	z = !1;
	try {
		return M.Yt === null ? e() : M.Yt(e);
	} finally {
		z = n;
	}
}
function qt(e, t) {
	e.ue & 512 ? (e.ue &= -513, Nt(e, !0)) : e.ue & 64 ? e.T & 32 && Nt(e, !0) : t && Pt(e);
}
function Jt(e, t) {
	let n = t.It;
	(n == null || I(n) !== e) && e.ct.add(t);
}
function Yt(e) {
	return E !== null && I(e) === I(E);
}
function Xt(e, t) {
	let n = e.Ge;
	if (n === null || Yt(n)) return !1;
	let r = I(n);
	Jt(r, t);
	let i = r.oe.get(e);
	return i ? i.add(t) : e.S & 1 && ze(r, () => t.C.notify(t, 1, 1, e.o._)), !0;
}
var Zt = null;
function Qt(e, t = e.Ge) {
	if (!t || t === E || e?.o?.Ht || B?.o?.Ht) return;
	let n = B;
	if (E === null && !F.Kt) {
		if (M.jt) return;
		if (n.ue & 4 && !(n.T & 128) && (Zt === null || Zt === t)) {
			Zt = t;
			return;
		}
	}
	F.initTransition(t);
}
function $t(e, t, n, r) {
	return !!(!t || V !== null && M.Bt(e, n, t) || e.ve === m || t.T & 16 || At && !r && Xt(e, t) || e.T & 131072 && !Mt && !(t.T & 8192));
}
var en = !1;
function tn() {
	en = !0;
}
function nn(e, t = e.Qe) {
	return F.Kt || e.ve === m || e.T & 4194304 || e.o?.Ht ? m : e.Ge === null || e.T & 16777216 ? t : e.o === null ? m : e.o.gt;
}
var rn = [], an = [];
function on(e) {
	return !F.Kt && e.o?.Ct === T && !e.o?.Ht;
}
function sn(e) {
	let t = e.o;
	return t !== null && t.Ce !== void 0 && t.Ce !== m;
}
function cn(e) {
	return sn(e) && !on(e);
}
function ln(e) {
	return e.ue |= a, !0;
}
var un = [];
function dn() {
	if (en = !1, rn.length !== 0) {
		for (let e of rn) e.o.gt = m;
		rn.length = 0;
	}
	if (an.length !== 0) {
		for (let e of an) e.T &= ~f;
		an.length = 0;
	}
	if (un.length !== 0) {
		for (let e of un) M.me(e, e.ve === m ? e.Qe : e.ve);
		un.length = 0;
	}
}
function fn(e) {
	if (Mt) return M.zt(e);
	let t = B;
	t?.xt && (t = t.Qt);
	let n = e, r = e.Te || e;
	if (typeof n.ce == "function" && qt(e, !1), !n.ce && r === e && e.o?.Ce === void 0 && e.o?.nt === void 0 && E === null && V === null && (!en || e.ve === m)) return t && z && ft(e, t), !t || e.ve === m || t.T & 16 || At && Xt(e, t) ? e.Qe : (Qt(e), e.ve);
	if (t && z && (ft(e, t, jt), r.ce)) {
		let n = L(e);
		r.tt >= n.et ? (Ge(t), We(n), Pt(r)) : t.T & 65536 && Pt(r);
		let i = r.tt;
		i >= t.tt && e._parent !== t && (t.tt = i + 1);
	}
	if (r.S & 1) {
		if (t && (!At || r.S & 4 || r.T & 2097152 || r.T & 1024 && M.Xt(r) || !Xt(r, t))) {
			if (V === null || M.$t(r)) throw !z && e !== t && ft(e, t), r.o?._;
		} else if (!t && r.S & 4) throw r.o?._;
	}
	if (r.ce && r.S & 2) {
		if (z && r.Pe < T) return Nt(r), fn(e);
		throw r.o?._;
	}
	let i = pn(e, t, r, e.Qe);
	return !t && r === e && typeof n.ce == "function" && e.T & 32 && !(r.S & 1) && !e.u && !cn(e) && (ut.add(e), A()), i;
}
function pn(t, n, r, i) {
	if (sn(t)) {
		if (!(n && n.T & 8192) && !on(t)) return n && t.T & 525312 ? M.en(t, n) : g(t.o?.Ce);
		t.T |= c;
	}
	if (V !== null && E !== null && n !== null && M.tn(t, r, n)) return i;
	let a = t.ve !== m && !!(t.S & 4);
	if (a && !n) throw new e(null);
	let o = n && en ? nn(t, i) : m;
	return o === m ? $t(t, n, r, a) ? i : (Qt(t), t.ve) : (ln(n), o);
}
function mn(e) {
	if (F.Kt) return;
	let t = H(e);
	t.gt === m && (t.gt = e.ve, rn.push(e), en = !0);
}
function hn(e) {
	F.Kt || e.T & 4194304 || (e.T |= f, an.push(e));
}
function gn(e, t) {
	if (e.Ge && E !== e.Ge && (F.Kt ? F.initTransition(e.Ge) : (fe.push(e.Ge), A())), e.T & 128) return M.ln(e, t);
	let n = e.ve === m ? e.Qe : e.ve;
	if (typeof t == "function" && (t = t(n)), !(e.S & 4 || !e.Fe || !e.Fe(n, t))) return t;
	let r = e.ve !== m;
	return r ? e.Ge !== null && mn(e) : N(e), e.ve = t, B !== null && hn(e), e.T & 256 && M.me !== null && (M.me(e, t), F.Kt || un.push(e)), e.ce !== void 0 && (e.Pe = T), r && e.ht === ye && V === null ? t : (Se(e), A(), t);
}
function _n(e) {
	Ue(e, L(e)), !(e.ue & 1024) && e.ve === m && (N(e), A()), e.ue = e.ue & -4 | r, e.sn = T;
}
function vn(e, t) {
	let n = gn(e, t);
	return _n(e), n;
}
function yn(e, t) {
	let n = B, r = z;
	B = e, z = !1;
	try {
		return t();
	} finally {
		B = n, z = r;
	}
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/effect.js
function bn(e, t, n, r) {
	let i = It(e, t, n, r?.user ? 2 : 1, r);
	Nt(i, !0), !r?.defer && i.ve === m && (i.Le === 2 || r?.schedule ? i.C.enqueue(i.Le, Sn.bind(null, i)) : Sn(i, 4));
}
function xn(e, t) {
	let r = e === void 0 ? this.S : e, i = t === void 0 ? this.o?._ : t;
	if (r & 2) {
		if (this.C.notify(this, 1, 0), this.Le === 2) {
			this.S & 2 && (this.Ye = !0, this.C.enqueue(this.Le, this.dt ??= Sn.bind(null, this)));
			return;
		}
		if (!this.C.notify(this, 2, 2)) throw me(n(i)), i;
	} else this.Le === 1 && this.C.notify(this, 3, r, i);
}
function Sn(e, r) {
	if (!e.Ye || e.ue & 64) return;
	if (e.It !== null && !I(e.It).Tt && (r & 4 ? !e.o?.Ue : E !== null)) {
		e.C.enqueue(e.Le, e.dt);
		return;
	}
	if (e.S & 2 && e.Le === 2) {
		let t = n(e.o?._);
		e.Ut = e.Qe, e.Ye = !1;
		try {
			e.Vt ? e.Vt(t, () => {
				let t = e.yt;
				e.yt = void 0, t?.();
			}) : console.error(t);
		} catch (t) {
			if (!e.C.notify(e, 2, 2)) throw me(t), t;
		}
		return;
	}
	let i = e.o?._ == null, a = e.yt;
	e.yt = void 0;
	try {
		a?.(), e.yt = e.Lt(e.Qe, e.Ut);
	} catch (n) {
		if (H(e)._ = new t(e, n), e.S |= 2, !e.C.notify(e, 2, 2)) throw me(n), n;
	} finally {
		e.Ut = e.Qe, e.Ye = !1, i && st(e);
	}
}
M.Et = Sn, Rt(xn);
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/signals.js
function Cn(e) {
	return nt(e);
}
function wn(e) {
	let t = fn.bind(null, e);
	return t[v] = e, t;
}
function Tn(e, t) {
	if (typeof e == "function") {
		let n = Ft(e, t);
		return n.T &= -33, [wn(n), vn.bind(null, n)];
	}
	let n = Ht(e, t);
	return [wn(n), gn.bind(null, n)];
}
function En(e, t) {
	return wn(Ft(e, t));
}
function Dn(e, t, n) {
	bn(e, t, void 0, n);
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/store/store.js
var On = Symbol(0), kn = Symbol(0);
function An(e) {
	return Reflect.ownKeys(e).filter((t) => Object.prototype.propertyIsEnumerable.call(e, t));
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/map.js
function jn(e, t, n) {
	let r = typeof n?.keyed == "function" ? n.keyed : void 0, i = t.length > 1, a = t, o = {
		se: it(),
		ts: 0,
		ss: e,
		es: [],
		rs: a,
		ns: [],
		hs: [],
		qt: r,
		fs: r || n?.keyed === !1 ? [] : void 0,
		cs: i && n?.keyed !== !1 ? [] : void 0,
		ls: n?.keyed === !1,
		us: n?.fallback
	}, s = Ft(Fn.bind(o), void 0);
	return o.se.Qt = s, s.T &= -33, wn(s);
}
var Mn = { ownedWrite: !0 };
function Nn(e, t, n, r) {
	let i = e.es, a = e.ts - 1, o = [], s = [], c = [], l = 256, u = r, d = r, f = !1;
	for (; u <= a && d <= n - 1;) {
		let e = i[u], r = t[d];
		if (e === r) {
			f ||= (c.push(u, d, 0), !0), c[c.length - 1]++, u++, d++;
			continue;
		}
		f = !1;
		let p = -1, m = Math.min(32 - o.length, a - u, l);
		for (let e = 1; e <= m; e++) if (i[u + e] === r) {
			p = e;
			break;
		}
		l -= p === -1 ? m : p;
		let h = -1;
		m = Math.min(32 - s.length, n - 1 - d, l);
		for (let n = 1; n <= m; n++) if (t[d + n] === e) {
			h = n;
			break;
		}
		if (l -= h === -1 ? m : h, p !== -1 && (h === -1 || p <= h)) {
			for (; p-- > 0;) o.push(u++);
			continue;
		}
		if (h !== -1) {
			for (; h-- > 0;) s.push(d++);
			continue;
		}
		if (l <= 0 || o.length === 32 || s.length === 32) return !1;
		o.push(u++), s.push(d++);
	}
	for (; u <= a; u++) {
		if (o.length === 32) return !1;
		o.push(u);
	}
	for (; d <= n - 1; d++) {
		if (s.length === 32) return !1;
		s.push(d);
	}
	return Pn(e, t, n, o, s, c);
}
function Pn(e, t, n, r, i, a) {
	let o = e.es, s, c, l;
	if (i.length !== 0) for (l = Array(r.length), c = 0; c < i.length; c++) {
		let e = -1;
		for (s = 0; s < r.length; s++) if (!l[s] && o[r[s]] === t[i[c]]) {
			e = s;
			break;
		}
		if (e === -1) return !1;
		l[e] = !0, i[c] = i[c] << 6 | e;
	}
	if (r.length !== 0 || i.length !== 0) {
		let e = /* @__PURE__ */ new Set();
		for (s = 0; s < r.length; s++) e.add(o[r[s]]);
		for (c = 0; c < i.length; c++) e.add(t[i[c] >> 6]);
		for (let t = 0; t < a.length; t += 3) {
			let n = a[t];
			for (let r = 0, i = a[t + 2]; r < i; r++) if (e.has(o[n + r])) return !1;
		}
	}
	let u = e.ns, d = e.hs, f = u.slice(0, n), p = d.slice(0, n);
	for (let e = 0; e < a.length; e += 3) {
		let t = a[e], n = a[e + 1];
		if (t !== n) for (let r = 0; r < a[e + 2]; r++) f[n + r] = u[t + r], p[n + r] = d[t + r];
	}
	for (c = 0; c < i.length; c++) {
		let e = i[c] >> 6, t = r[i[c] & 63];
		f[e] = u[t], p[e] = d[t];
	}
	for (e.ns = f, e.hs = p, e.ts = n, e.es = t.slice(0), s = 0; s < r.length; s++) (l === void 0 || !l[s]) && d[r[s]].dispose();
	return !0;
}
function Fn() {
	let e = this.ss() || [], t = e.length;
	return e[On], yn(this.se, () => {
		let n, r, i, a, o = this.fs ? this.ls ? () => (i[r] = Ht(e[r], Mn), this.rs(wn(i[r]), r)) : () => (i[r] = Ht(e[r], Mn), a && (a[r] = Ht(r, Mn)), this.rs(wn(i[r]), a ? wn(a[r]) : void 0)) : this.cs ? () => {
			let t = e[r];
			return a[r] = Ht(r, Mn), this.rs(t, wn(a[r]));
		} : () => {
			let t = e[r];
			return this.rs(t);
		};
		if (t === 0) this.ts !== 0 && (this.se.dispose(!1), this.hs = [], this.es = [], this.ns = [], this.ts = 0, this.fs &&= [], this.cs &&= []), this.us && !this.ns[0] && (this.hs[0]?.dispose(), this.ns[0] = yn(this.hs[0] = it(), this.us));
		else if (this.ts === 0) {
			let s = Array(t), c = Array(t);
			i = this.fs && Array(t), a = this.cs && Array(t);
			try {
				for (r = 0; r < t; r++) s[r] = yn(c[r] = it(), o);
			} catch (e) {
				for (n = 0; n <= r; n++) c[n]?.dispose();
				throw e;
			}
			this.hs[0] && this.hs[0].dispose(), this.ns = s, this.hs = c, i && (this.fs = i), a && (this.cs = a), this.es = e.slice(0), this.ts = t;
		} else {
			let s, c, l, u, d, f, p, m, h;
			for (s = 0, c = Math.min(this.ts, t); s < c && (this.es[s] === e[s] || this.fs && In(this.qt, this.es[s], e[s])); s++) this.fs && gn(this.fs[s], e[s]);
			for (c = this.ts - 1, l = t - 1; c >= s && l >= s && (this.es[c] === e[l] || this.fs && In(this.qt, this.es[c], e[l])); c--, l--);
			if (s === t && this.ts === t) {
				this.es = e.slice(0);
				return;
			}
			if (t <= this.ts && c - s > 64 && this.fs === void 0 && this.cs === void 0) {
				let n = s + (l - s >> 1), r = e[n], i = Math.min(c, n + 32), a = Math.max(s, n - 32);
				for (; a <= i && this.es[a] !== r;) a++;
				if (a <= i && Nn(this, e, t, s)) return;
			}
			let g = t - this.ts, _ = Array(t), v = Array(t);
			for (i = this.fs ? Array(t) : void 0, a = this.cs ? Array(t) : void 0, f = /* @__PURE__ */ new Map(), p = Array(l + 1), r = l; r >= s; r--) u = e[r], d = this.qt ? this.qt(u) : u, n = f.get(d), p[r] = n === void 0 ? -1 : n, f.set(d, r);
			for (n = s; n <= c; n++) u = this.es[n], d = this.qt ? this.qt(u) : u, r = f.get(d), r !== void 0 && r !== -1 ? (_[r] = this.ns[n], v[r] = this.hs[n], i && (i[r] = this.fs[n]), a && (a[r] = this.cs[n]), r = p[r], f.set(d, r)) : (m ??= []).push(this.hs[n]);
			try {
				for (r = s; r <= l; r++) v[r] === void 0 && ((h ??= []).push(v[r] = it()), _[r] = yn(v[r], o));
			} catch (e) {
				if (h) for (n = 0; n < h.length; n++) h[n].dispose();
				throw e;
			}
			for (n = 0; n < s; n++) _[n] = this.ns[n], v[n] = this.hs[n], i && (i[n] = this.fs[n]), a && (a[n] = this.cs[n]);
			for (r = s; r <= l; r++) i && gn(i[r], e[r]), a && gn(a[r], r);
			for (r = l + 1; r < t; r++) _[r] = this.ns[r - g], v[r] = this.hs[r - g], i && (i[r] = this.fs[r - g], gn(i[r], e[r])), a && (a[r] = this.cs[r - g], g !== 0 && gn(a[r], r));
			if (this.ns = _, this.hs = v, i && (this.fs = i), a && (this.cs = a), this.ts = t, this.es = e.slice(0), m) for (n = 0; n < m.length; n++) m[n].dispose();
		}
	}), this.ns;
}
function In(e, t, n) {
	return !e || e(t) === e(n);
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/boundaries.js
function Ln(e, t) {
	if (typeof e == "function" && !e.length) {
		if (t?.doNotUnwrap) return e;
		do
			e = e();
		while (typeof e == "function" && !e.length);
	}
	if (!t?.skipNonRendered || e != null && e !== !0 && e !== !1 && e !== "") {
		if (Array.isArray(e)) {
			let n = [];
			return Rn(e, n, t) ? () => {
				let e = [];
				return Rn(n, e, {
					...t,
					doNotUnwrap: !1
				}), e;
			} : n;
		}
		return e;
	}
}
function Rn(t, n = [], r) {
	let i = null, a = !1;
	for (let o = 0; o < t.length; o++) try {
		let e = t[o];
		if (typeof e == "function" && !e.length) {
			if (r?.doNotUnwrap) {
				n.push(e), a = !0;
				continue;
			}
			do
				e = e();
			while (typeof e == "function" && !e.length);
		}
		Array.isArray(e) ? a = Rn(e, n, r) || a : r?.skipNonRendered && (e == null || e === !0 || e === !1 || e === "") || n.push(e);
	} catch (t) {
		if (!(t instanceof e)) throw t;
		i = t;
	}
	if (i) throw i;
	return a;
}
var zn = Object.freeze({});
function Bn(e, t) {
	return t === 3 ? (e = e()) ?? zn : e;
}
function Vn(e, t) {
	let n = e.hidden;
	return typeof n == "function" ? n(t) : n.includes(t);
}
function Hn(e) {
	return Bn(e.source, e.kind);
}
function Un(e, t) {
	return t === 0 ? Object.keys(e) : t === 2 || e[kn] === e ? Reflect.ownKeys(e) : Object.keys(e);
}
function Wn(e, t) {
	if (t === 1) {
		if (e.kind === 4) return Kn(e.source, !1, e);
		let t = Un(Hn(e), e.kind), n = [];
		for (let r = 0; r < t.length; r++) Vn(e, t[r]) || n.push(t[r]);
		return n;
	}
	return Un(Bn(e, t), t);
}
function Gn(e, t) {
	if (e !== void 0) {
		for (let n = e.length - 1; n >= 0; n--) if (Vn(e[n], t)) return !0;
	}
	return !1;
}
function Kn(e, t, n) {
	let r = [];
	return qn(e, n === void 0 ? void 0 : [n], t, r, null), r;
}
function qn(e, t, n, r, i) {
	let a = e.sources, o = e.kinds;
	for (let e = 0; e < a.length; e++) {
		let s = a[e], c = o[e], l;
		if (c === 1) {
			if (s.kind === 4) {
				t === void 0 ? t = [s] : t.push(s), qn(s.source, t, n, r, i), t.pop();
				continue;
			}
			l = s, c = s.kind, s = s.source;
		}
		s = Bn(s, c);
		let u = n ? An(s) : Un(s, c);
		for (let e = 0; e < u.length; e++) {
			let n = u[e];
			l !== void 0 && Vn(l, n) || Gn(t, n) || Jn(r, i, n, s);
		}
	}
}
function Jn(e, t, n, r) {
	let i = e.indexOf(n);
	i !== -1 && (e.splice(i, 1), t !== null && t.splice(i, 1)), e.push(n), t !== null && t.push(r);
}
//#endregion
//#region node_modules/.pnpm/solid-js@2.0.0-rc.9/node_modules/solid-js/dist/solid.js
var Yn = !1, Xn = {
	hydrating: !1,
	registry: void 0,
	done: !1
}, Zn = (...e) => En(...e), U = (...e) => Tn(...e), Qn = (...e) => at(...e), $n = (...e) => Dn(...e);
function W(e, t, n) {
	return Kt(() => e(t || {}));
}
var er = (e) => `Stale read from <${e}>.`;
function tr(e) {
	let t = "fallback" in e ? {
		keyed: e.keyed,
		fallback: () => e.fallback
	} : { keyed: e.keyed }, n = tt(), r, i = () => yn(n, () => jn(() => e.each, e.children, t));
	return Xn.hydrating && (r = i()), () => (r ??= i())();
}
function G(e) {
	let t = e.keyed, n = En(() => e.when, void 0), r = t ? n : En(n, {
		equals: (e, t) => !e == !t,
		sync: !0
	});
	return En(() => {
		let i = r();
		if (i) {
			let a = e.children;
			return typeof a == "function" && a.length > 0 ? Kt(t ? () => a(i) : () => a(() => {
				if (!Kt(r)) throw er("Show");
				return n();
			}), Yn) : a;
		}
		return e.fallback;
	}, { sync: !0 });
}
//#endregion
//#region node_modules/.pnpm/@solidjs+web@2.0.0-rc.9_solid-js@2.0.0-rc.9/node_modules/@solidjs/web/dist/web.js
var K = /*#__PURE__*/ Symbol("slot"), nr = /*#__PURE__*/ Symbol("host"), rr = {
	transparent: !0,
	sync: !0
}, ir = { sync: !0 };
function q(e, t, n) {
	$n(e, t, n ? {
		sync: !0,
		...n,
		transparent: !n.scope
	} : rr);
}
function J(e) {
	return Zn(() => e(), ir);
}
function ar(e, t, n, r) {
	let i = n.length, a = t.length, o = i, s = 0, c = 0, l = t[a - 1], u = l[K], d = l.parentNode === e && (!u || u === r) ? l.nextSibling : r || null, f = null, p, m, h = (t) => {
		if (!t) return !1;
		let n = t[K];
		return t.parentNode === e && (!n || n === r);
	};
	for (; s < a || c < o;) {
		if (t[s] === n[c] && h(t[s])) {
			s++, c++;
			continue;
		}
		for (; t[a - 1] === n[o - 1] && h(t[a - 1]);) a--, o--;
		if (a === s) {
			let t;
			if (o < i) {
				if (c) {
					let i = n[c - 1], a = i[K];
					t = i.parentNode === e && (!a || a === r) ? i.nextSibling : d;
				} else t = n[o - c];
			} else t = d;
			for (; c < o;) {
				let i = n[c++];
				e.insertBefore(i, t), r && (i[K] = r);
			}
		} else if (o === c) for (; s < a;) {
			let n = t[s++];
			if (!f || !f.has(n)) {
				let t = n[K];
				n.parentNode === e && (!t || t === r) && n.remove();
			}
		}
		else if ((p = t[s]) === n[o - 1] && n[c] === t[a - 1] && p.parentNode === e && (!(m = p[K]) || m === r)) {
			if (r) do {
				let n = t[--a];
				if (e.insertBefore(n, p), n[K] = r, c++, s >= a - 1 || c >= o) break;
			} while (t[s] === n[o - 1] && n[c] === t[a - 1]);
			else do
				if (e.insertBefore(t[--a], p), c++, s >= a - 1 || c >= o) break;
			while (t[s] === n[o - 1] && n[c] === t[a - 1]);
		} else {
			if (!f) {
				f = /* @__PURE__ */ new Map();
				let e = c;
				for (; e < o;) f.set(n[e], e++);
			}
			let i = f.get(t[s]);
			if (i != null) {
				if (c < i && i < o) {
					let l = s, u = 1, p;
					for (; ++l < a && l < o && (p = f.get(t[l])) != null && p === i + u;) u++;
					if (u > i - c) {
						let a = t[s], o = a[K], l = a.parentNode === e && (!o || o === r) ? a : d;
						for (; c < i;) {
							let t = n[c++];
							e.insertBefore(t, l), r && (t[K] = r);
						}
					} else {
						let i = t[s++], a = n[c++], o = i[K];
						i.parentNode === e && (!o || o === r) ? e.replaceChild(a, i) : e.insertBefore(a, d), r && (a[K] = r);
					}
				} else s++;
			} else {
				let n = t[s++], i = n[K];
				n.parentNode === e && (!i || i === r) && n.remove();
			}
		}
	}
}
var or = "_$$", sr = "_$SOLID_EVENT_OWNER", cr = {}, lr = /* @__PURE__ */ new Set(), ur = /* @__PURE__ */ new Map();
function dr(e, t, n, r = {}) {
	let i;
	mr(t);
	try {
		Qn((a) => {
			if (i = a, r.onError && (tt()[pe] = r.onError), t === document) {
				let t = e();
				q(() => Ln(t), () => {});
			} else {
				let i = e();
				Q(t, () => i, t.firstChild ? null : void 0, n, {
					...r.insertOptions,
					schedule: !0
				});
			}
		}, { id: r.renderId }), Me();
	} catch (e) {
		throw i && i(), hr(t), e;
	}
	return () => {
		i(), hr(t), t.textContent = "";
	};
}
function fr(e, t, n) {
	let r = document.createElement("template");
	return r.innerHTML = e, n === 2 ? r.content.firstChild.firstChild : r.content.firstChild;
}
function Y(e, t) {
	let n;
	return t === 1 ? (r) => document.importNode(n ||= fr(e, r, t), !0) : (r) => (n ||= fr(e, r, t)).cloneNode(!0);
}
function pr(e) {
	for (let t = 0, n = e.length; t < n; t++) {
		let n = e[t];
		lr.has(n) || (lr.add(n), ur.forEach((e, t) => vr(n, t, e)));
	}
}
function mr(e) {
	let t = gr(e, e);
	t && (t.roots = (t.roots || 0) + 1);
}
function hr(e) {
	let t = ur.get(e);
	t && (t.roots > 1 ? t.roots-- : delete t.roots), _r(e, e);
}
function gr(e, t = e) {
	if (!e || !t) return;
	let n = ur.get(e);
	return n || ur.set(e, n = {
		owners: /* @__PURE__ */ new Map(),
		handlers: /* @__PURE__ */ new Map()
	}), n.owners.set(t, (n.owners.get(t) || 0) + 1), lr.forEach((t) => vr(t, e, n)), n;
}
function _r(e, t = e) {
	let n = ur.get(e);
	if (!n) return;
	let r = n.owners.get(t);
	r > 1 ? n.owners.set(t, r - 1) : n.owners.delete(t), !n.owners.size && (n.handlers.forEach((t, n) => e.removeEventListener(n, t)), ur.delete(e));
}
function vr(e, t, n) {
	if (n.handlers.has(e)) return;
	let r = (e) => jr(e, t, n);
	n.handlers.set(e, r), t.addEventListener(e, r);
}
function yr(e, t) {
	let n = e, r = 0;
	for (; n;) {
		if (t.owners.has(n)) return {
			owner: n,
			distance: r
		};
		r++, n = n._$host || n.parentNode || n.host;
	}
}
var br = null;
function xr(e) {
	if (br !== null) for (let t = 0; t < br.length; t++) br[t](e);
	return e;
}
function X(e, t, n) {
	if (Or(e)) return;
	let r = t === "multiple" && e.localName === "select";
	if (n == null || n === !1) e.removeAttribute(t);
	else if (e.setAttribute(t, n === !0 ? "" : n), r && !e._$multiple) {
		let t = e.options;
		for (let e = 0; e < t.length; e++) t[e].defaultSelected && (t[e].selected = !0);
	}
	r && (e._$multiple = !0), br !== null && (t === "href" || t === "action") && xr(e);
}
function Sr(e, t, n) {
	if (typeof t == "number" && (t = "" + t), typeof n == "number" && (n = "" + n), Or(e)) {
		e._$classes = t && typeof t == "object" ? kr(t) : void 0;
		return;
	}
	if (t == null || t === !1) {
		(n || e._$classes) && (e.removeAttribute("class"), e._$classes = void 0);
		return;
	}
	if (typeof t == "string") {
		e._$classes = void 0, t !== n && e.setAttribute("class", t);
		return;
	}
	let r;
	typeof n == "string" ? (r = {}, e.removeAttribute("class")) : r = e._$classes || kr(n || {}), t = kr(t);
	let i = Object.keys(t), a = Object.keys(r), o, s;
	for (o = 0, s = a.length; o < s; o++) {
		let n = a[o];
		n && n !== "undefined" && !t[n] && e.classList.remove(n);
	}
	for (o = 0, s = i.length; o < s; o++) {
		let n = i[o], a = !!t[n];
		n && n !== "undefined" && r[n] !== a && a && e.classList.add(n);
	}
	e._$classes = t;
}
function Cr(e) {
	if (typeof e != "object" || !e) return e;
	if (Array.isArray(e)) return e.map(Cr);
	if (e[kn] !== e) return e;
	let t = Wn(e, 2), n = {};
	for (let r = 0; r < t.length; r++) {
		let i = t[r];
		typeof i == "string" && (n[i] = e[i]);
	}
	return n;
}
function wr(e, t, n) {
	Or(e) || (n == null ? e.style.removeProperty(t) : e.style.setProperty(t, n));
}
function Tr(e, t) {
	Array.isArray(e) ? e.flat(Infinity).forEach((e) => e && e(t)) : e(t);
}
function Er(e, t) {
	let n = Kt(e);
	yn(null, () => Tr(n, t));
}
var Dr = { scope: !0 }, Z = null;
function Q(e, t, n, r, i) {
	let a = n !== void 0, o = i && i.host;
	if (a && !r && (r = []), Z !== null && (r = Z.claimInitial(e, a, r)), typeof t != "function" && (t = Nr(t, r, a, !0), typeof t != "function")) {
		Mr(e, t, r, n), o && Pr(t, o);
		return;
	}
	if (a && r.length === 0) {
		let t = document.createTextNode("");
		e.insertBefore(t, n), r = [t];
	}
	let s = r;
	q((r) => {
		Z !== null && (s = Z.reclaimRegion(s, e, n));
		let c = Nr(t(), s, a, !0);
		return typeof c == "function" ? (q(() => (Z !== null && (s = Z.reclaimRegion(s, e, n)), Nr(c, s, a)), (t) => {
			s = Mr(e, t, s, n), o && Pr(s, o);
		}, r !== void 0 && !(i && i.schedule) ? {
			...i,
			schedule: !0
		} : i), cr) : c;
	}, (t) => {
		t !== cr && (s = Mr(e, t, s, n), o && Pr(s, o));
	}, t.$s ? i ? {
		...i,
		scope: !0
	} : Dr : i);
}
function Or(e) {
	if (!Xn.hydrating || Xn.isClaiming && !Xn.isClaiming()) return !1;
	if (!e || e.isConnected) return !0;
	let t = Xn.claimRoots;
	if (t) {
		for (let n = 0; n < t.length; n++) if (t[n].contains(e)) return !0;
	}
	return !1;
}
function kr(e) {
	if (Array.isArray(e)) {
		let t = {};
		Ar(e, t), e = t;
	}
	if (e && typeof e == "object") {
		let t = {}, n = Object.keys(e);
		for (let r = 0, i = n.length; r < i; r++) {
			let i = n[r];
			if (!e[i]) continue;
			let a = i.trim().split(/\s+/);
			for (let e = 0, n = a.length; e < n; e++) a[e] && (t[a[e]] = !0);
		}
		return t;
	}
	return e;
}
function Ar(e, t) {
	for (let n = 0, r = e.length; n < r; n++) {
		let r = e[n];
		Array.isArray(r) ? Ar(r, t) : typeof r == "object" && r ? Object.assign(t, r) : typeof r != "boolean" && (r || r === 0) && (t[r] = !0);
	}
}
function jr(e, t, n) {
	if (Z !== null && Z.dedupEvent(e)) return;
	let r = e[sr], i;
	if (r) {
		if (r === !0 || r === t || !t.contains(r)) return;
		i = r;
	}
	let a = n && (n.owners.size === 1 && n.owners.has(t) ? t : yr(e.target, n)?.owner);
	if (n && !a || a && a === i) return;
	e[sr] = a || !0;
	let o = i || e.target, s = or + e.type, c = e.target, l = a || t || e.currentTarget, u = (t) => Object.defineProperty(e, "target", {
		configurable: !0,
		value: t
	}), d = () => {
		let t = o[s];
		if (t === void 0 && o.hasAttribute && o.hasAttribute("_bnd")) {
			let n = globalThis[Symbol.for("solid.bnd")];
			n && (t = n.resolve(o, e.type));
		}
		if (t && !o.disabled) {
			let n = o[`${s}Data`];
			if (n === void 0 ? typeof t == "function" ? t.call(o, e) : t.handleEvent(e) : t.call(o, n, e), e.cancelBubble) return;
		}
		return o.host && typeof o.host != "string" && !o.host._$host && o.contains(e.target) && u(o.host), !0;
	}, f = () => {
		for (; o && d() && o !== l && o.parentNode !== l;) o = o._$host || o.parentNode || o.host;
	};
	if (Object.defineProperty(e, "currentTarget", {
		configurable: !0,
		get() {
			return o || l || document;
		}
	}), i) i === e.target && (o = i._$host || i.parentNode || i.host), o && o !== l && f();
	else if (e.composedPath) {
		let t = e.composedPath();
		if (t.length) {
			u(t[0]);
			for (let e = 0; e < t.length && (o = t[e], d()); e++) {
				if (o._$host) {
					o = o._$host, f();
					break;
				}
				if (o === l || o.parentNode === l) break;
			}
		} else f();
	} else f();
	u(c);
}
function Mr(e, t, n, r) {
	if (Z !== null && Or(e)) {
		if (t && t !== n) {
			let e = Array.isArray(t);
			for (let r of e ? t : [t]) if (r && r.nodeType) {
				if (!Or(r)) return n;
			} else if (e && (typeof r == "string" || typeof r == "number")) return n;
		}
		return t;
	}
	if (t === n) return t;
	let i = typeof t, a = r !== void 0;
	if (i === "string" || i === "number") {
		let r = typeof n;
		r === "string" || r === "number" ? e.firstChild.data = t : Ir(e, n) ? e.textContent = t : (Lr(e, n), e.insertBefore(document.createTextNode(t), e.firstChild));
	} else if (t === void 0) Rr(e, n, r);
	else if (t.nodeType) Array.isArray(n) ? Rr(e, n, a ? r : null, t) : n && n.nodeType ? n.parentNode === e ? e.replaceChild(t, n) : e.appendChild(t) : n && e.firstChild ? e.replaceChild(t, e.firstChild) : e.appendChild(t), r && (t[K] = r);
	else if (Array.isArray(t)) {
		let i = n && Array.isArray(n);
		for (let e = 0, r = t.length; e < r; e++) {
			let r = t[e], a = typeof r;
			if (a === "string" || a === "number") {
				let a = i ? n[e] : void 0;
				a && a.nodeType === 3 ? (a.data !== "" + r && (a.data = r), t[e] = a) : t[e] = document.createTextNode(r);
			}
		}
		t.length === 0 ? Rr(e, n, r) : i ? n.length === 0 ? Fr(e, t, r) : ar(e, n, t, r) : (n && Rr(e, n), Fr(e, t));
	}
	return t;
}
function Nr(e, t, n, r) {
	if (e = Ln(e, {
		skipNonRendered: !0,
		doNotUnwrap: r
	}), r && typeof e == "function") return e;
	if (n && !Array.isArray(e) && (e = [e ?? ""]), Xn.hydrating && Array.isArray(e)) for (let n = 0, r = e.length; n < r; n++) {
		let r = e[n], i = t && t[n], a = typeof r;
		(a === "string" || a === "number") && i && i.nodeType === 3 && Or(i) && (e[n] = i);
	}
	return e;
}
function Pr(e, t) {
	if (Array.isArray(e)) for (let n = 0, r = e.length; n < r; n++) Pr(e[n], t);
	else e && e.nodeType && e[nr] !== t && (e[nr] = t, Object.defineProperty(e, "_$host", {
		get: t,
		configurable: !0
	}));
}
function Fr(e, t, n = null) {
	for (let r = 0, i = t.length; r < i; r++) {
		let i = t[r];
		e.insertBefore(i, n), n && (i[K] = n);
	}
}
function Ir(e, t) {
	if (t == null) return !0;
	if (Array.isArray(t)) return t.length ? e.firstChild === t[0] && e.lastChild === t[t.length - 1] : e.firstChild === null;
	if (t === "") return e.firstChild === null;
	if (t.nodeType) return e.firstChild === t && e.lastChild === t;
	let n = e.firstChild;
	return n !== null && n.nodeType === 3 && e.lastChild === n;
}
function Lr(e, t) {
	if (Array.isArray(t)) for (let n = 0; n < t.length; n++) {
		let r = t[n];
		r.parentNode === e && r.remove();
	}
	else if (t.nodeType) t.parentNode === e && t.remove();
	else {
		let t = e.firstChild;
		t && t.nodeType === 3 && t.remove();
	}
}
function Rr(e, t, n, r) {
	if (n === void 0) return Ir(e, t) ? e.textContent = "" : Lr(e, t);
	if (t.length) {
		let i = !1;
		for (let a = t.length - 1; a >= 0; a--) {
			let o = t[a];
			if (r !== o) {
				let t = o[K], s = o.parentNode === e && (!t || t === n);
				r && !i && !a ? s ? e.replaceChild(r, o) : e.insertBefore(r, n) : s && o.remove();
			} else i = !0;
		}
	} else r && e.insertBefore(r, n);
	r && n && (r[K] = n);
}
//#endregion
//#region src/main.tsx
var zr = /* @__PURE__ */ Y("<svg class=pos-symbol-defs aria-hidden=true><defs><clipPath id=pos-coin-large-fragment><path d=\"M0 0h21.5l-3.5 9.5 3.5 5.9L17.8 32H0Z\"></path></clipPath><symbol id=pos-coin viewBox=\"0 0 32 32\"><path d=\"M4 12.5v6c0 5 5.4 9 12 9s12-4 12-9v-6\"fill=currentColor fill-opacity=.24 stroke=currentColor stroke-width=1.6 stroke-linejoin=round></path><path d=\"M8 22.5v3M16 24.5v3M24 22.5v3\"fill=none stroke=currentColor stroke-opacity=.55 stroke-width=1></path><ellipse cx=16 cy=12.5 rx=12 ry=9 fill=#fff stroke=currentColor stroke-width=1.6></ellipse><path d=\"M9.5 15.7V9.4l6.5 5 6.5-5v6.3\"fill=none stroke=#ff6600 stroke-width=2.5 stroke-linecap=square></path><path d=\"M9.5 15.7v1.2h13v-1.2\"fill=none stroke=currentColor stroke-width=1></path></symbol><symbol id=pos-coin-partial viewBox=\"0 0 32 32\"><use href=#pos-coin clip-path=url(#pos-coin-large-fragment)></use><path d=\"M21.5 4 18 9.5 21.5 15.4 17.8 27\"fill=none stroke=currentColor stroke-width=1.2 stroke-linejoin=round></path><path d=\"M21.5 4.2C25.6 6 28 8.9 28 12.5v6c0 4.7-4.2 8.1-10.2 8.5\"fill=none stroke=currentColor stroke-width=1.4 stroke-dasharray=\"2.2 2.2\"stroke-linecap=round></path></symbol><symbol id=pos-coins-overpaid viewBox=\"0 0 45 32\"><use href=#pos-coin x=0 y=3 width=31 height=29></use><use href=#pos-coin x=13 y=0 width=31 height=29>"), Br = /* @__PURE__ */ Y("<span class=pos-spinner>"), Vr = /* @__PURE__ */ Y("<span class=\"pos-disc pos-disc-empty\">"), Hr = /* @__PURE__ */ Y("<span class=pos-disc>"), Ur = /* @__PURE__ */ Y("<svg><use>"), Wr = /* @__PURE__ */ Y("<svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=1.8><path d=\"M5 2h14M5 22h14M7 2v4c0 3 5 5 5 6s-5 3-5 6v4m10-20v4c0 3-5 5-5 6s5 3 5 6v4M8 18h8l2 3H6z\">"), Gr = /* @__PURE__ */ Y("<span class=pos-exclaim>!"), Kr = /* @__PURE__ */ Y("<svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=1.8 stroke-linecap=round><path d=\"M3 9a15 15 0 0 1 18 0M6 12a10 10 0 0 1 5-1M18 12l1 1M9 16a5 5 0 0 1 6 0M12 20h.01M3 3l18 18\">"), qr = /* @__PURE__ */ Y("<span class=pos-cross>×"), Jr = /* @__PURE__ */ Y("<span aria-hidden=true><!><!><!><!><!><!><!><!>"), Yr = /* @__PURE__ */ Y("<span><!><!>"), Xr = /* @__PURE__ */ Y("<button class=pos-back aria-label=\"Back to POS\">←"), Zr = /* @__PURE__ */ Y("<span class=pos-store>POS"), Qr = /* @__PURE__ */ Y("<header class=pos-top><span class=pos-chevron>›</span><strong></strong><span class=pos-health>"), $r = /* @__PURE__ */ Y("<section class=pos-stack aria-label=\"Background orders\"><div class=pos-stack-heading><strong>Background orders · </strong><button>View all →</button></div><div class=pos-stack-scroll>"), ei = /* @__PURE__ */ Y("<p class=pos-error role=alert>"), ti = /* @__PURE__ */ Y("<main><div class=pos-amount><span></span></div><div class=pos-keys></div><label class=pos-ref-label for=pos-reference>Reference <span>(optional)</span></label><input id=pos-reference class=pos-reference type=text maxlength=120 placeholder=\"e.g. a name or order\"autocomplete=off><button class=pos-primary>"), ni = /* @__PURE__ */ Y("<p>Loading orders…"), ri = /* @__PURE__ */ Y("<p>Searching orders…"), ii = /* @__PURE__ */ Y("<p class=pos-error role=alert> <button>Retry"), ai = /* @__PURE__ */ Y("<p class=pos-empty>"), oi = /* @__PURE__ */ Y("<button class=pos-load-more>Load more orders"), si = /* @__PURE__ */ Y("<main class=pos-list><h1>Background orders</h1><p class=pos-list-subtitle>Keep track of every order while you serve the next customer.</p><input type=search aria-label=\"Search reference or order ID\"placeholder=\"Search reference or order ID\"><div class=pos-tabs role=tablist aria-label=\"Order status\"><button role=tab>Active · <!><!></button><button role=tab>Finished · <!><!></button></div><!><!><!><div class=pos-list-items></div><!>"), ci = /* @__PURE__ */ Y("<a class=pos-store>"), li = /* @__PURE__ */ Y("<button><strong class=pos-stack-ref></strong><span>"), ui = /* @__PURE__ */ Y("<button>"), di = /* @__PURE__ */ Y("<svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=2 stroke-linecap=round stroke-linejoin=round aria-hidden=true><path d=\"M8 5 2 12l6 7h13a1 1 0 0 0 1-1V6a1 1 0 0 0-1-1H8Z\"></path><path d=\"m12 9 5 6m0-6-5 6\">"), fi = /* @__PURE__ */ Y("<div class=pos-checkout-card><iframe>"), pi = /* @__PURE__ */ Y("<button class=pos-primary>Background order"), mi = /* @__PURE__ */ Y("<button class=pos-cancel>Cancel order"), hi = /* @__PURE__ */ Y("<p class=pos-action-hint>Keep the payment open while you serve the next customer."), gi = /* @__PURE__ */ Y("<button class=pos-primary>New order"), _i = /* @__PURE__ */ Y("<main class=pos-payment><div class=pos-order-heading><div><h1></h1><p>Order </p></div></div><!><!><!><!>"), vi = /* @__PURE__ */ Y("<section class=pos-outcome><p></p><p> <!> · <!> XMR"), yi = /* @__PURE__ */ Y("<article class=pos-order-card><div class=pos-order-card-head><div><h2></h2><p><!> · created <!></p></div></div><div class=pos-order-sum> <span></span></div><div class=pos-order-foot><small></small><button>Open →"), bi = document.getElementById("pos-root");
if (!bi) throw Error("POS root missing");
var xi = {
	connectionId: bi.dataset.connectionId || "",
	publicKey: bi.dataset.publicKey || "",
	currency: bi.dataset.currency || "AUD",
	decimals: Number(bi.dataset.decimals || "2"),
	storeName: bi.dataset.storeName || "Store"
}, Si = `/dashboard/stores/${encodeURIComponent(xi.connectionId)}/pos`, $ = (e) => !!e.cancelled_at || [
	"paid",
	"overpaid",
	"expired"
].includes(e.status), Ci = (e, t = !1) => t ? "offline" : e.cancelled_at ? "cancelled" : e.error?.includes("Double-spend") ? "double-spend" : e.status === "confirming" && e.confirmations === 0 ? "unconfirmed" : e.status, wi = {
	pending: "Awaiting payment",
	unconfirmed: "Unconfirmed",
	confirming: "Confirming",
	partial: "Partially paid",
	paid: "Paid",
	overpaid: "Overpaid",
	expired: "Expired",
	cancelled: "Cancelled",
	"double-spend": "Double spend",
	offline: "Connection lost"
}, Ti = (e) => e.length <= 14 ? e : `${e.slice(0, 6)}…${e.slice(-4)}`, Ei = (e) => e.merchant_order_id || Ti(e.order_id), Di = (e) => {
	let t = e.padStart(xi.decimals + 1, "0");
	return `${t.slice(0, -xi.decimals) || "0"}.${t.slice(-xi.decimals)}`;
}, Oi = (e) => {
	let [t, n] = Di(e).split(".");
	return `${t.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}.${n}`;
};
async function ki(e, t) {
	let n = await fetch(e, t);
	if (!n.ok) {
		let e = await n.json().catch(() => ({}));
		throw Error(e.error || `Request failed (${n.status})`);
	}
	return n.status === 204 ? void 0 : n.json();
}
var Ai = (e, t) => ki(e, {
	method: "POST",
	headers: t === void 0 ? void 0 : { "content-type": "application/json" },
	body: t === void 0 ? void 0 : JSON.stringify(t)
});
function ji() {
	return zr();
}
function Mi(e) {
	let t = () => Ci(e.order, e.offline), n = () => e.order.confirmations_required <= 0 ? 100 : Math.min(100, Math.max(20, Math.ceil(10 * e.order.confirmations / e.order.confirmations_required) * 10));
	var r = Jr(), i = r.firstChild, a = i.nextSibling, o = a.nextSibling, s = o.nextSibling, c = s.nextSibling, l = c.nextSibling, u = l.nextSibling, d = u.nextSibling;
	return Q(r, W(G, {
		get when() {
			return t() === "pending";
		},
		get children() {
			return Br();
		}
	}), i), Q(r, W(G, {
		get when() {
			return t() === "unconfirmed";
		},
		get children() {
			return Vr();
		}
	}), a), Q(r, W(G, {
		get when() {
			return t() === "confirming";
		},
		get children() {
			var e = Hr();
			return q(() => `${n()}%`, (t) => {
				wr(e, "--progress", t);
			}), e;
		}
	}), o), Q(r, W(G, {
		get when() {
			return t() === "partial" || t() === "paid" || t() === "overpaid";
		},
		get children() {
			var e = Ur(), n = e.firstChild;
			return q(() => ({
				e: t() === "overpaid" ? "0 0 45 32" : "0 0 32 32",
				t: t() === "partial" ? "#pos-coin-partial" : t() === "overpaid" ? "#pos-coins-overpaid" : "#pos-coin"
			}), ({ e: t, t: r }, i) => {
				t !== i?.e && X(e, "viewBox", t), r !== i?.t && X(n, "href", r);
			}), e;
		}
	}), s), Q(r, W(G, {
		get when() {
			return t() === "expired";
		},
		get children() {
			return Wr();
		}
	}), c), Q(r, W(G, {
		get when() {
			return t() === "double-spend";
		},
		get children() {
			return Gr();
		}
	}), l), Q(r, W(G, {
		get when() {
			return t() === "offline";
		},
		get children() {
			return Kr();
		}
	}), u), Q(r, W(G, {
		get when() {
			return t() === "cancelled";
		},
		get children() {
			return qr();
		}
	}), d), q(() => `pos-icon pos-icon-${t()}`, (e, t) => {
		Sr(r, e, t);
	}), r;
}
function Ni(e) {
	var t = Yr(), n = t.firstChild, r = n.nextSibling;
	return Q(t, W(Mi, {
		get order() {
			return e.order;
		},
		get offline() {
			return e.offline;
		}
	}), n), Q(t, () => wi[Ci(e.order, e.offline)] || e.order.status, r), q(() => `pos-badge state-${Ci(e.order, e.offline)}`, (e, n) => {
		Sr(t, e, n);
	}), t;
}
function Pi() {
	let [e, t] = U([]), [n, r] = U("keypad"), [i, a] = U(null), [o, s] = U("0"), [c, l] = U(""), [u, d] = U(""), [f, p] = U(!1), [m, h] = U(!0), [g, _] = U(!1), [v, y] = U(""), [b, ee] = U([]), [te, ne] = U(0), [re, x] = U(0), [S, C] = U(!1), [w, T] = U("active"), [E, D] = U(0), [O, ie] = U(0), [ae, oe] = U(0), k = Zn(() => e().find((e) => e.order_id === i()) || null), se = Zn(() => e().filter((e) => !$(e) && e.order_id !== i())), ce = Zn(() => e()), le = Zn(() => ce().filter((e) => !$(e)).length), ue = Zn(() => ce().filter($).length), A = Zn(() => (v().trim() ? b() : ce()).filter((e) => w() === "active" !== $(e))), j = null, de, fe, pe = 0, me, he = null;
	function ge(e) {
		t((t) => t.some((t) => t.order_id === e.order_id) ? t.map((t) => t.order_id === e.order_id ? t.updated_at !== void 0 && e.updated_at !== void 0 && e.updated_at < t.updated_at ? t : e : t) : [e, ...t]), ee((t) => t.map((t) => t.order_id === e.order_id ? e : t));
	}
	async function _e(e = !1) {
		let t = v().trim();
		if (!t) return;
		let n = pe;
		C(!0);
		let r = e ? re() : 0;
		try {
			let i = await ki(`${Si}/orders?offset=${r}&limit=40&search=${encodeURIComponent(t)}`);
			if (n !== pe) return;
			ne(i.total), x(r + i.orders.length), ee((t) => e ? [...t, ...i.orders.filter((e) => !t.some((t) => t.order_id === e.order_id))] : i.orders), d(""), queueMicrotask(P);
		} catch (e) {
			n === pe && d(e.message);
		} finally {
			n === pe && C(!1);
		}
	}
	function M(e) {
		pe++, y(e), window.clearTimeout(fe), e.trim() ? (ee([]), x(0), ne(0), C(!0), d(""), fe = window.setTimeout(() => void _e(), 200)) : (ee([]), x(0), ne(0), C(!1), d(""), queueMicrotask(P));
	}
	async function N(e = !1) {
		try {
			let n = e ? O() : 0, o = await ki(`${Si}/orders?offset=${n}&limit=40`);
			if (D(o.total), ie(n + o.orders.length), t((t) => e ? [...t, ...o.orders.filter((e) => !t.some((t) => t.order_id === e.order_id))] : o.orders), !e && !i()) {
				let e = o.orders.find((e) => !e.backgrounded && !$(e));
				e && (a(e.order_id), r("payment"));
			}
			d(""), queueMicrotask(P);
		} catch (e) {
			d(e.message);
		} finally {
			h(!1);
		}
	}
	async function ve(e) {
		let t = await ki(`${Si}/orders/${encodeURIComponent(e)}`);
		return ge(t), t;
	}
	function ye() {
		let t = k() && !$(k()) ? [k().order_id] : [], r = n() === "list" ? A().filter((e) => !$(e)).map((e) => e.order_id) : se().map((e) => e.order_id), i = e().filter((e) => !$(e)).map((e) => e.order_id), a = e().filter((e) => e.cancelled_at && e.status === "pending").slice(0, 8).map((e) => e.order_id);
		return [.../* @__PURE__ */ new Set([
			...t,
			...r,
			...i,
			...a
		])].slice(0, 32);
	}
	function P() {
		j?.close(), j = null;
		let e = ye();
		if (!e.length) {
			_(!1);
			return;
		}
		let n = new EventSource(`${Si}/events?orders=${e.map(encodeURIComponent).join(",")}`);
		j = n, n.addEventListener("open", () => {
			j === n && (window.clearTimeout(de), _(!1), be());
		}), n.addEventListener("status", (e) => {
			if (j === n) try {
				let n = JSON.parse(e.data);
				t((e) => e.map((e) => e.order_id === n.order_id ? e.updated_at !== void 0 && n.updated_at !== void 0 && n.updated_at < e.updated_at ? e : {
					...e,
					...n
				} : e)), ee((e) => e.map((e) => e.order_id === n.order_id ? e.updated_at !== void 0 && n.updated_at !== void 0 && n.updated_at < e.updated_at ? e : {
					...e,
					...n
				} : e)), n.is_terminal && queueMicrotask(P);
			} catch {}
		}), n.addEventListener("error", () => {
			j === n && (window.clearTimeout(de), de = window.setTimeout(() => _(!0), 6e3));
		});
	}
	async function be() {
		let e = ye();
		(await Promise.allSettled(e.map(ve))).some((e) => e.status === "rejected") && _(!0);
	}
	function xe() {
		s("0"), l(""), a(null), r("keypad"), d("");
	}
	function Se(e) {
		s((t) => (t + e).slice(-(xi.decimals + 9)).replace(/^0+(?=\d)/, "") || "0");
	}
	function Ce() {
		s((e) => e.length > 1 ? e.slice(0, -1) : "0");
	}
	async function we() {
		if (f() || /^0+$/.test(o())) return;
		p(!0), d("");
		let e = Di(o()), t = c().trim();
		(!he || he.amount !== e || he.reference !== t) && (he = {
			amount: e,
			reference: t,
			key: crypto.randomUUID()
		});
		try {
			let n = await ki(`${Si}/orders`, {
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({
					amount: e,
					merchant_order_id: t || null,
					request_key: he.key
				})
			});
			await ve(n.order_id), he = null, a(n.order_id), r("payment"), queueMicrotask(P);
		} catch (e) {
			d(e.message);
		} finally {
			p(!1);
		}
	}
	async function Te() {
		let e = k();
		if (e && !f()) {
			p(!0), d("");
			try {
				await Ai(`${Si}/orders/${encodeURIComponent(e.order_id)}/background`), ge({
					...e,
					backgrounded: !0
				}), xe(), queueMicrotask(P);
			} catch (e) {
				d(e.message);
			} finally {
				p(!1);
			}
		}
	}
	async function Ee() {
		let e = k();
		if (e && !f() && window.confirm(`Cancel ${Ei(e)}? The payment address has already been issued; any later payment will still need review.`)) {
			p(!0), d("");
			try {
				await Ai(`${Si}/orders/${encodeURIComponent(e.order_id)}/cancel`), await ve(e.order_id), queueMicrotask(P);
			} catch (e) {
				d(e.message);
			} finally {
				p(!1);
			}
		}
	}
	async function De(e) {
		n() === "list" && me && oe(me.scrollTop), d(""), a(e.order_id), r("payment");
		try {
			await ve(e.order_id), queueMicrotask(P);
		} catch (e) {
			d(e.message);
		}
	}
	function Oe() {
		r("list"), queueMicrotask(() => {
			me && (me.scrollTop = ae()), P();
		});
	}
	function ke(e) {
		if (n() === "keypad") {
			if (e.target instanceof HTMLInputElement) {
				e.key === "Enter" && we();
				return;
			}
			/^[0-9]$/.test(e.key) ? Se(e.key) : e.key === "Backspace" ? Ce() : e.key === "Escape" ? s("0") : e.key === "Enter" && we();
		}
	}
	return document.addEventListener("keydown", ke), queueMicrotask(() => {
		N();
	}), Cn(() => {
		document.removeEventListener("keydown", ke), j?.close(), window.clearTimeout(de), window.clearTimeout(fe);
	}), [
		W(ji, {}),
		(() => {
			var e = Qr(), t = e.firstChild, i = t.nextSibling, a = i.nextSibling;
			return Q(e, W(G, {
				get when() {
					return n() === "list";
				},
				get fallback() {
					var e = ci();
					return xr(e), Q(e, () => xi.storeName), q(() => `/dashboard/stores/${encodeURIComponent(xi.connectionId)}`, (t) => {
						X(e, "href", t);
					}), e;
				},
				get children() {
					return [(() => {
						var e = Xr();
						return e._$$click = () => {
							r("keypad"), queueMicrotask(P);
						}, e;
					})(), Zr()];
				}
			}), t), Q(i, () => n() === "list" ? "Orders" : "POS"), q(() => g() ? "Connection lost" : "Connected", (e) => {
				X(a, "aria-label", e);
			}), e;
		})(),
		W(G, {
			get when() {
				return J(() => n() === "keypad")() && ce().length > 0;
			},
			get children() {
				var e = $r(), t = e.firstChild, n = t.firstChild;
				n.firstChild;
				var r = n.nextSibling, i = t.nextSibling;
				return Q(n, () => se().length, null), r._$$click = Oe, i.addEventListener("wheel", (e) => {
					e.currentTarget.scrollWidth > e.currentTarget.clientWidth && Math.abs(e.deltaY) > Math.abs(e.deltaX) && (e.currentTarget.scrollLeft += e.deltaY, e.preventDefault());
				}), Q(i, W(tr, {
					get each() {
						return se();
					},
					children: (e) => (() => {
						var t = li(), n = t.firstChild, r = n.nextSibling;
						return t._$$click = () => void De(e), Q(t, W(Mi, {
							order: e,
							get offline() {
								return g();
							}
						}), n), Q(n, () => Ei(e)), Q(r, () => e.amount), q(() => ({
							e: `pos-stack-card state-${Ci(e, g())}`,
							t: `${Ei(e)} · ${wi[Ci(e, g())]} · ${e.amount} ${e.currency}`,
							a: `Open ${Ei(e)}, ${wi[Ci(e, g())]}, ${e.amount} ${e.currency}`
						}), ({ e, t: n, a: r }, i) => {
							Sr(t, e, i?.e), n !== i?.t && X(t, "title", n), r !== i?.a && X(t, "aria-label", r);
						}), t;
					})()
				})), e;
			}
		}),
		W(G, {
			get when() {
				return n() === "keypad";
			},
			get children() {
				var e = ti(), t = e.firstChild, n = t.firstChild, r = t.nextSibling, i = r.nextSibling.nextSibling, a = i.nextSibling;
				return Q(t, () => Oi(o()), n), Q(n, () => xi.currency), Q(r, W(tr, {
					each: [
						"1",
						"2",
						"3",
						"4",
						"5",
						"6",
						"7",
						"8",
						"9",
						"C",
						"0",
						"⌫"
					],
					children: (e) => (() => {
						var t = ui();
						return t._$$click = () => e === "C" ? s("0") : e === "⌫" ? Ce() : Se(e), X(t, "aria-label", e === "C" ? "Clear" : e === "⌫" ? "Backspace" : e), Q(t, e === "⌫" ? di() : e), q(() => Cr(e === "C" ? "clear" : e === "⌫" ? "delete" : ""), (e, n) => {
							Sr(t, e, n);
						}), t;
					})()
				})), i._$$input = (e) => l(e.currentTarget.value), Q(e, W(G, {
					get when() {
						return u();
					},
					get children() {
						var e = ei();
						return Q(e, u), e;
					}
				}), a), a._$$click = () => void we(), Q(a, () => f() ? "Creating order…" : "Charge"), q(() => ({
					e: `pos-keypad ${ce().length ? "has-stack" : ""}`,
					t: c(),
					a: /^0+$/.test(o()) || f()
				}), ({ e: t, t: n, a: r }, o) => {
					Sr(e, t, o?.e), i.value = n ?? "", r !== o?.a && X(a, "disabled", r);
				}), e;
			}
		}),
		W(G, {
			get when() {
				return J(() => n() === "payment")() ? i() : null;
			},
			keyed: !0,
			children: (e) => (() => {
				var e = _i(), t = e.firstChild, n = t.firstChild.firstChild, r = n.nextSibling;
				r.firstChild;
				var i = t.nextSibling, a = i.nextSibling, o = a.nextSibling, s = o.nextSibling;
				return Q(n, () => Ei(k())), Q(r, () => Ti(k().order_id), null), Q(t, W(Ni, {
					get order() {
						return k();
					},
					get offline() {
						return g();
					}
				}), null), Q(e, W(G, {
					get when() {
						return J(() => !k().cancelled_at)() && !$(k());
					},
					get fallback() {
						var e = vi(), t = e.firstChild, n = t.nextSibling, r = n.firstChild, i = r.nextSibling, a = i.nextSibling.nextSibling;
						return a.nextSibling, Q(e, W(Ni, { get order() {
							return k();
						} }), t), Q(t, (() => {
							var e = J(() => !!k().cancelled_at);
							return () => e() ? "This order was cancelled. If money arrives at its address, review the payment in the order details." : k().error || "This order is finished.";
						})()), Q(n, () => k().amount, r), Q(n, () => k().currency, i), Q(n, () => k().xmr_amount, a), e;
					},
					get children() {
						var e = fi(), t = e.firstChild;
						return q(() => ({
							e: `Payment and refund details for ${Ei(k())}`,
							t: `/pay/${encodeURIComponent(xi.publicKey)}/orders/${encodeURIComponent(k().order_id)}?view=compact`
						}), ({ e, t: n }, r) => {
							e !== r?.e && X(t, "title", e), n !== r?.t && X(t, "src", n);
						}), e;
					}
				}), i), Q(e, W(G, {
					get when() {
						return u();
					},
					get children() {
						var e = ei();
						return Q(e, u), e;
					}
				}), a), Q(e, W(G, {
					get when() {
						return !$(k());
					},
					get children() {
						return [
							(() => {
								var e = pi();
								return e._$$click = () => void Te(), q(() => f(), (t) => {
									X(e, "disabled", t);
								}), e;
							})(),
							W(G, {
								get when() {
									return J(() => k().status === "pending")() && !k().error;
								},
								get children() {
									var e = mi();
									return e._$$click = () => void Ee(), q(() => f(), (t) => {
										X(e, "disabled", t);
									}), e;
								}
							}),
							hi()
						];
					}
				}), o), Q(e, W(G, {
					get when() {
						return $(k());
					},
					get children() {
						var e = gi();
						return e._$$click = xe, e;
					}
				}), s), e;
			})()
		}),
		W(G, {
			get when() {
				return n() === "list";
			},
			get children() {
				var e = si(), t = e.firstChild.nextSibling.nextSibling, n = t.nextSibling, r = n.firstChild, i = r.firstChild.nextSibling, a = i.nextSibling, o = r.nextSibling, s = o.firstChild.nextSibling, c = s.nextSibling, l = n.nextSibling, d = l.nextSibling, f = d.nextSibling, p = f.nextSibling, h = p.nextSibling;
				return Er(() => (e) => {
					me = e;
				}, e), t._$$input = (e) => M(e.currentTarget.value), r._$$click = () => {
					T("active"), queueMicrotask(P);
				}, Q(r, le, i), Q(r, () => O() < E() ? "+" : "", a), o._$$click = () => {
					T("finished"), queueMicrotask(P);
				}, Q(o, ue, s), Q(o, () => O() < E() ? "+" : "", c), Q(e, W(G, {
					get when() {
						return m();
					},
					get children() {
						return ni();
					}
				}), l), Q(e, W(G, {
					get when() {
						return S();
					},
					get children() {
						return ri();
					}
				}), d), Q(e, W(G, {
					get when() {
						return u();
					},
					get children() {
						var e = ii(), t = e.firstChild, n = t.nextSibling;
						return Q(e, u, t), n._$$click = () => void N(), e;
					}
				}), f), Q(e, W(G, {
					get when() {
						return J(() => !(m() || S() || u()))() ? A().length === 0 : !m() && !S() && !u();
					},
					get children() {
						var e = ai();
						return Q(e, (() => {
							var e = J(() => !!v());
							return () => e() ? "No matching orders." : `No ${w()} background orders yet.`;
						})()), e;
					}
				}), p), Q(p, W(tr, {
					get each() {
						return A();
					},
					children: (e) => (() => {
						var t = yi(), n = t.firstChild, r = n.firstChild.firstChild, i = r.nextSibling, a = i.firstChild, o = a.nextSibling, s = o.nextSibling, c = n.nextSibling, l = c.firstChild, u = l.nextSibling, d = c.nextSibling.firstChild, f = d.nextSibling;
						return Q(r, () => Ei(e)), Q(i, () => e.merchant_order_id ? "Reference · " : "", a), Q(i, () => Ti(e.order_id), o), Q(i, () => (/* @__PURE__ */ new Date(e.created_at * 1e3)).toLocaleTimeString([], {
							hour: "2-digit",
							minute: "2-digit"
						}), s), Q(n, W(Ni, {
							order: e,
							get offline() {
								return J(() => !!g())() ? !$(e) : g();
							}
						}), null), Q(c, () => e.amount, l), Q(u, () => e.currency), Q(d, (() => {
							var t = J(() => !!(e.cancelled_at && e.status !== "pending"));
							return () => t() ? "Payment activity after cancellation — review" : e.error || (e.status === "partial" ? "Waiting for remaining amount" : e.status === "confirming" ? `${e.confirmations} of ${e.confirmations_required} confirmations` : wi[Ci(e)]);
						})()), f._$$click = () => void De(e), t;
					})()
				})), Q(e, W(G, {
					get when() {
						return J(() => !!v().trim())() ? re() < te() : O() < E();
					},
					get children() {
						var e = oi();
						return e._$$click = () => void (v().trim() ? _e(!0) : N(!0)), e;
					}
				}), h), q(() => ({
					e: v(),
					t: w() === "active" ? "true" : "false",
					a: Cr(w() === "active" ? "selected" : ""),
					o: w() === "finished" ? "true" : "false",
					i: Cr(w() === "finished" ? "selected" : "")
				}), ({ e, t: n, a: i, o: a, i: s }, c) => {
					t.value = e ?? "", n !== c?.t && X(r, "aria-selected", n), Sr(r, i, c?.a), a !== c?.o && X(o, "aria-selected", a), Sr(o, s, c?.i);
				}), e;
			}
		})
	];
}
dr(() => W(Pi, {}), bi), pr(["click", "input"]);
//#endregion

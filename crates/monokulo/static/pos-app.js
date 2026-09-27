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
	if (cn(e) && e.o?.Ft) {
		let t = V(e).Ft = L(e.o?.Ft);
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
			i !== n && (!cn(e) || e.T & 8388608) && (n.Ln && b(n.Ln) === i ? (V(e).Ue = t, e.T |= o) : i.Ln && b(i.Ln) === n || ee(n, i));
			return;
		}
	}
	V(e).Ue = t, e.T |= o;
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
	if (e.ue & 128) return F.We(e);
	e.ue & 16 ? e.ue &= -12 : (We(e, C), e.ue &= -4);
}
var T = 0, E = null, D = !1, O = !1, k = !1, A = 0, ie = 0, j = /* @__PURE__ */ new Set();
function ae(e) {
	let t = e.m;
	return x.size === 0 && y.size === 0 && e.hn.length === 0 && t.rt.length === 0 && t.A.length === 0 && t.dn.size === 0 && j.size === 0;
}
function oe() {
	if (j.size !== 0) for (let e of j) {
		if (e.u !== null) {
			j.delete(e);
			continue;
		}
		e.ve === m && (e.o?.Ce === void 0 || e.o?.Ce === m) && (e.o?.t || (j.delete(e), e.T & 262144 ? Wt(e) : e.o?.Pt?.()));
	}
}
function se() {
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
function ce(e, t) {
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
function M() {
	if (O) {
		fe();
		return;
	}
	D || (D = !0, !A && !I.Kt && queueMicrotask(je));
}
var N = [];
function le() {
	for (let e of x) N.includes(e) || N.push(e);
	M();
}
var P = [], ue = Symbol.for("solid-js/root-error-hook");
function de(e) {
	if (O) return;
	O = !0;
	let t = "[REACTIVITY_HALTED]", n = e !== void 0 && globalThis.reportError;
	n || e === void 0 ? console.error(t) : console.error(t, e), n && n(e);
}
function fe() {
	k || (k = !0, console.error("[REACTIVITY_HALTED]"));
}
var pe = 0, me = class {
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
			this.mn[e - 1] = [], Me(t, e);
		}
		let t = this.hn, n = ++pe;
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
		e && (B ? b(B).fn[e - 1].push(t) : this.mn[e - 1].push(t)), M();
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
}, F = class e extends me {
	Kt = !1;
	m = se();
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
			if (E === null && S.EE < S.et && this.mn[0].length === 0 && this.mn[1].length === 0 && this.hn.length === 0 && !N.length && !P.length && ae(this)) {
				this.Kt = !0;
				try {
					fn(), ft(), Ee();
				} finally {
					this.Kt = !1;
				}
				T++, D = S.EE >= S.et || this.mn[0].length !== 0 || this.mn[1].length !== 0 || this.m.Ot.length !== 0;
				return;
			}
			this.Kt = !0, fn();
			try {
				for (; P.length;) this.initTransition(P.pop());
				if (ft(), qe(S, e.We), E) {
					if (e.Tn?.(E) && qe(S, e.We), !Fe(E)) {
						let t = E;
						Te.length = 0, qe(C, this.m === t ? w : e.We), this.m === t && (Ae = this.m = se()), y.size && (e._n(1), e._n(2)), this.stashQueues(t.Sn), T++, D = S.EE >= S.et || this.m.Ot.length > 0, ke(t.Ot), E = null, De(null, !0);
						return;
					}
					let t = E, n = this.m;
					if (n !== t && n.Ot.push(...t.Ot), this.restoreQueues(t.Sn), x.delete(t), E = null, ke(n.Ot), De(t), n === t) {
						let e = se();
						e.Ot = n.Ot, e.rt = n.rt, e.A = n.A, e.dn = n.dn, Ae = this.m = e;
					}
				} else ae(this) ? (Ee(), S.EE >= S.et && (qe(S, e.We), Ee())) : (x.size && qe(C, e.We), De());
				T++, D = S.EE >= S.et || E !== null, y.size && e._n(1), this.run(1), y.size && e._n(2), this.run(2);
			} finally {
				for (; !D && !E && N.length;) this.initTransition(N.pop());
				this.Kt = !1;
			}
		}
	}
	notify(t, n, r, i) {
		if (n & 1) {
			if (r & 1) {
				let n = i ?? t.o?._;
				if (n?.l) return !0;
				if (n && (!E && !t.Ge && Ae.Ot.length && this.initTransition(), E)) {
					let r = n.source, i = E.oe.get(r);
					i || E.oe.set(r, i = /* @__PURE__ */ new Set());
					let a = i.size;
					i.add(t), i.size !== a && (M(), e.vn?.(E));
				}
			}
			return !0;
		}
		return !1;
	}
	initTransition(e) {
		if (e && (e = L(e), e.Tt === !0 || e === E) || !e && E && E.Pe === T) return;
		if (!E) E = e ?? se();
		else if (e) {
			let t = E;
			ce(e, t), this.restoreQueues(t.Sn), x.delete(t), E = e;
		}
		x.add(E), E.Pe = T;
		let t = this.m;
		if (t !== E) {
			let e = this.Kt ? 0 : p;
			for (let n = 0; n < t.Ot.length; n++) {
				let r = t.Ot[n];
				if (r.Ge === null && r.ve !== m && (!r.ce || r.ue & 1024 && !(r.S & 4)) && r.Fe && r.Fe(r.Qe, r.ve)) {
					r.ve = m, Se(r);
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
			Ae = this.m = E;
		}
		for (let e of y) e.Ge ||= E;
		M();
	}
};
function he(e) {
	Ae.Ot.push(e), I.Kt || nn();
}
var ge = !1, _e = 0;
function ve() {
	_e++;
}
var ye = 0;
function be(e) {
	let t = ye;
	return ye = e, t;
}
function xe(e, t = !1) {
	e.ht = _e;
	let n = e.T, r = (n & 1024 ? e.o?.Ue : void 0) || B, o = !!(n & 512) && e.o?.nt !== void 0, s = ge;
	for (let n = e.u; n !== null; n = n.Ne) {
		let e = n._e;
		if (s && (e.ue &= ~i), e.ue & 4 && n.qe === e.Ze && n !== e.ot && (e.ue |= a), o && e.T & 8) {
			e.ue |= 256;
			continue;
		}
		t && r ? (e.ue |= 128, re(e, r)) : t && (e.ue |= 128, e.o && (e.o.Ue = void 0)), Be(e);
	}
}
function Se(e) {
	let t = e;
	if (!t.ce) {
		e.ve !== m && (e.Qe = e.ve, e.ve = m), e.T & 256 && F.En(e);
		return;
	}
	e.ve !== m && (e.Qe = e.ve, e.ve = m, t.S &= -5, e.Le && e.Le !== 3 && (e.Ye = !0), e.o && (e.o.be = !1)), t.ge = !1, t.ue &= ~r, t.o?._ ?? ct(t), t.T &= ~u, t.S & 1 ? e.T |= d : t.S &= -5, t.o != null && (t.o.lt !== null || t.o.it !== null) && F.Be(t, !1, !0), e.T & 256 && F.En(e);
}
var Ce = null, we = [], Te = [];
function Ee() {
	for (; Te.length;) ct(Te.pop());
	let e = Ae.Ot;
	for (let t = 0; t < e.length; t++) {
		let n = e[t];
		Se(n), n.Ge = null, n.T & 131072 && (n.T &= ~l, we.push(n));
	}
	e.length = 0, Ce?.();
}
function De(e = null, t = !1) {
	let n = Ae, r = !t;
	r && Ee(), !t && I.hn.length && Oe(I);
	let i = e?.St, a = r && (e ?? n).rt.length !== 0;
	if (i && !a) for (let e of i) e.ue & 64 || Be(e);
	let o = S.EE >= S.et;
	if (o && qe(S, F.We), r) {
		if (Ae !== n) {
			if (e === null || e === n) return;
		} else o && Ee();
		let t = e ?? n;
		if (t.rt.length && F.On(t.rt), i && a) {
			for (let e of i) e.ue & 64 || Be(e);
			M();
		}
		if (t.ct.size) {
			for (let e of t.ct) e.ue & 64 || Be(e);
			t.ct.clear(), M();
		}
		if (t.A.length && (F.G(t.A), I.hn.length && Oe(I)), t.dn.size && F.Cn(t.dn, e), we.length !== 0) {
			for (; we.length;) xe(we.pop());
			S.EE >= S.et && (qe(S, F.We), Ee());
		}
		oe(), y.size && F.Rn(e);
	}
}
function Oe(e) {
	for (let t of e.hn) t.fe?.(), Oe(t);
}
function ke(e) {
	for (let t = 0; t < e.length; t++) e[t].Ge = E, e[t].T &= ~p;
}
var I = new F(), Ae = I.m;
function je(e) {
	if (ie > 0) return e ? e() : void 0;
	if (e) {
		A++;
		try {
			return e();
		} finally {
			try {
				je();
			} finally {
				A--;
			}
		}
	}
	if (!I.Kt && !O) {
		for (; D || E;) I.flush();
		ye = 0;
	}
}
function Me(e, t) {
	for (let n = 0; n < e.length; n++) e[n](t);
}
function Ne(t, n, r) {
	let i = t.ue;
	if (i & 64) return !1;
	if (i & 32) {
		let e = t;
		for (; e && e.ue & 32;) e = e._parent;
		let n = e && (e.Ge || (e.T & 1048576 ? E : null));
		if (!n || (n = L(n)).Tt === !0 || n === r) return !1;
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
function Pe(e, t, n) {
	let r = e.oe.get(t), i = !1;
	for (let e of r ?? []) {
		if (Ne(e, t, n)) return !0;
		n && e.ue & 32 ? i = !0 : r.delete(e);
	}
	return i || e.oe.delete(t), !1;
}
function Fe(e) {
	if (e.Tt) return !0;
	if (e.pe.length) return !1;
	let t = !0;
	for (let n of e.oe.keys()) if (Pe(e, n, e) && n.o?.ae?.size) {
		t = !1;
		break;
	}
	return t && F.Nn?.(e) && (t = !1), t && (e.Tt = !0), t;
}
function L(e) {
	for (; e.Tt && typeof e.Tt == "object";) e = e.Tt;
	return e;
}
function Ie(e) {
	for (let t of x) if (Pe(t, e)) return t;
	return null;
}
function Le(e) {
	for (let t of x) Pe(t, e) && I.initTransition(t);
}
function Re(e, t) {
	let n = E;
	try {
		return E = L(e), t();
	} finally {
		E = n;
	}
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/heap.js
function ze(e) {
	return e.ue & 32 ? C : S;
}
function Be(e) {
	let t = ze(e);
	t.et > e.tt && (t.et = e.tt), He(e, t);
}
function Ve(e, t) {
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
function He(e, t) {
	let n = e.ue;
	n & 1036 || (n & 1 ? e.ue = n & -4 | 10 : (e.ue = n | 8, t.tE && Ke(e)), n & 16 || Ve(e, t));
}
function Ue(e, t) {
	let n = e.ue;
	n & 1052 || (e.ue = n | 16, Ve(e, t));
}
function We(e, t) {
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
function Ge(e) {
	if (!e.tE) {
		e.tE = !0;
		for (let t = 0; t <= e.EE; t++) for (let n = e.eE[t]; n !== void 0; n = n.At) n.ue & 8 && Ke(n);
	}
}
function Ke(e, t = 2) {
	let n = e.ue;
	if (!((n & 3) >= t)) {
		e.ue = n & -4 | t;
		for (let t = e.u; t !== null; t = t.Ne) Ke(t._e, 1);
		if (e.T & 4096) for (let t = e.o.i; t !== null; t = t.De) for (let e = t.u; e !== null; e = e.Ne) Ke(e._e, 1);
	}
}
function qe(e, t) {
	for (e.tE = !1, e.et = 0; e.et <= e.EE; e.et++) {
		let n = e.eE[e.et];
		for (; n !== void 0;) n.ue & 8 ? t(n) : Je(n, e), n = e.eE[e.et];
	}
	e.EE = 0;
}
function Je(e, t) {
	We(e, t);
	let n = e.tt;
	for (let t = e.Se; t; t = t.de) {
		let e = t.Ee, r = e.Te || e;
		r.ce && r.tt >= n && (n = r.tt + 1);
	}
	if (e.tt !== n) {
		e.tt = n;
		for (let t = e.u; t !== null; t = t.Ne) Ue(t._e, ze(t._e));
	}
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/owner.js
function Ye(e) {
	let t = e.Xe;
	for (; t;) {
		let e = t.ue;
		t.ue = e | 32, e & 24 && (We(t, e & 32 ? C : S), e & 8 ? He(t, C) : Ue(t, C)), Ye(t), t = t.$e;
	}
}
function Xe(e, t = !1, n) {
	let r = e.ue;
	if (r & 64) return;
	if (t) {
		e.ue = r | 64;
		let t = e;
		(t.o?.je || t.o?.xe) && F.En(t), t.T & 2048 && t.o.bt.forEach(F.En);
		let n = t.Ge;
		n && t.S & 1 && !N.includes(n) && (N.push(n), M());
	}
	t && e.ce && e.o !== null && (e.o.Re = null);
	let i = n ? e.o?.lt ?? null : e.Xe;
	for (; i;) {
		let e = i.$e, t = i;
		t.T &= -33, We(t, ze(t)), lt(t), Xe(i, !0), i = e;
	}
	if (n ? e.o !== null && (e.o.lt = null) : (e.Xe = null, e.ut = 0), t && !n && !(r & 32) && e._parent !== null && !(e._parent.ue & 64)) {
		let t = e.Dt, n = e.$e;
		t === null ? e._parent.Xe = n : t.$e = n, n !== null && (n.Dt = t), e.Dt = null;
	}
	if (Ze(e, n), t && e.yt) {
		let t = e.yt;
		e.yt = void 0, t();
	}
}
function Ze(e, t) {
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
function Qe(e, t) {
	let n = e;
	for (; n.T & 4 && n._parent;) n = n._parent;
	if (n.id != null) return tt(n.id, t ? n.ut++ : n.ut);
	throw Error("");
}
function $e(e) {
	return Qe(e, !0);
}
function et(e, t, n) {
	return e?.id ?? (t ? n?.id : n?.id == null ? void 0 : $e(n));
}
function tt(e, t) {
	let n = t.toString(36), r = n.length - 1;
	return e + (r ? String.fromCharCode(64 + r) : "") + n;
}
function nt() {
	return z;
}
function rt(e) {
	return z && (z.ke ? Array.isArray(z.ke) ? z.ke.push(e) : z.ke = [z.ke, e] : z.ke = e), e;
}
function it(e = !0) {
	Xe(this, e);
}
function at(e) {
	let t = z, n = e?.transparent ?? !1, r = {
		id: et(e, n, t),
		T: n ? 4 : 0,
		xt: !0,
		Qt: t?.xt ? t.Qt : t,
		Xe: null,
		$e: null,
		Dt: null,
		ke: null,
		C: t?.C ?? I,
		ze: t?.ze || _,
		ut: 0,
		o: null,
		_parent: t,
		dispose: it
	};
	if (t) {
		let e = t.Xe;
		e === null ? t.Xe = r : (r.$e = e, e.Dt = r, t.Xe = r);
	}
	return r;
}
function ot(e, t) {
	let n = at(t);
	return bn(n, () => e(() => n.dispose()));
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/graph.js
function st(e) {
	let t = e.Ee, n = e.de, r = e.Ne, i = e.rn;
	if (r === null ? t.Gt = i : r.rn = i, i !== null) i.Ne = r;
	else if (t.u = r, r === null) {
		t.T & 262144 ? Wt(t) : t.o?.Pt?.();
		let e = t;
		e.ce && e.T & 32 && !(e.ue & 32) && !(e.S & 1) && ut(e);
	}
	return n;
}
function ct(e) {
	let t = e.ot, n = t === null ? e.Se : t.de;
	if (n !== null) {
		do
			n = st(n);
		while (n !== null);
		t === null ? e.Se = null : t.de = null;
	}
}
function lt(e) {
	let t = e.Se;
	if (t) {
		do
			t = st(t);
		while (t !== null);
		e.Se = null, e.ot = null;
	}
}
function ut(e) {
	We(e, ze(e)), lt(e), Xe(e, !0);
}
var dt = /* @__PURE__ */ new Set();
function ft() {
	if (dt.size !== 0) {
		for (let e of dt) !e.u && e.T & 32 && !(e.S & 1) && !(e.ue & 96) && ut(e);
		dt.clear();
	}
}
function pt(e, t, n = !1) {
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
	r === null ? t.Se = s : r.de = s, o === null ? e.u = s : o.Ne = s, ve();
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/async.js
function mt(e, t) {
	return !e.o?.ae?.has(t) && ((V(e).ae ??= /* @__PURE__ */ new Set()).add(t), !0);
}
function ht(e, t) {
	let n = e.o?.ae;
	return n?.delete(t) ? (n.size || (e.o.ae = void 0), !0) : !1;
}
function gt(e) {
	e.o !== null && (e.o.ae = void 0);
}
function _t(e, t) {
	for (let n = e.Se; n; n = n.de) {
		let e = n.Ee.Te || n.Ee;
		if (e === t || e.o?.ae?.has(t)) return !0;
	}
	return !1;
}
function vt(e, t) {
	V(e).Ie = !0, t.source && mt(e, t.source), e.S & 2 || yt(e, t.source, t);
}
function yt(t, n, r) {
	if (!n) {
		t.o !== null && (t.o._ = null);
		return;
	}
	if (r instanceof e && r.source === n) {
		V(t)._ = r;
		return;
	}
	let i = t.o?._;
	(!(i instanceof e) || i.source !== n) && (V(t)._ = new e(n));
}
function bt(e, t) {
	for (let n = e.u; n !== null; n = n.Ne) t(n._e, n);
	for (let n = e.o?.i ?? null; n !== null; n = n.De) for (let e = n.u; e !== null; e = e.Ne) t(e._e, e);
}
function xt(e) {
	e.ce && e.T & 32 && !e.u && !(e.ue & 32) && !(e.S & 1) && ut(e);
}
function St(e) {
	let t, n = /* @__PURE__ */ new Set(), r = (e) => {
		n.has(e) || (n.add(e), !e.u && e.T & 32 && (t ??= []).push(e), bt(e, r));
	};
	if (bt(e, r), t) for (let e of t) xt(e);
}
function Ct(e, t) {
	let n = !1, r = /* @__PURE__ */ new Set(), i = (e) => {
		r.has(e) || (r.add(e), e.o?._ === t && (Be(e), n = !0), bt(e, i));
	};
	bt(e, i), n && M();
}
function wt(e, t = e) {
	ht(e, t);
	let n = !1, r, i = /* @__PURE__ */ new Set(), a = F.Oe, o = (s) => {
		if (i.has(s) || t !== e && _t(s, t) || !ht(s, t)) return;
		i.add(s), s.Pe = T;
		let c = s.o?.ae?.values().next().value, l = s.S & 2;
		c ? (l || yt(s, c), a?.(s)) : (s.S &= -2, l || yt(s), a?.(s), s.o?.Ie && (Be(s), n = !0), s.o !== null && (s.o.Ie = !1), !s.u && s.T & 32 && (r ??= []).push(s)), bt(s, o);
	};
	if (bt(e, o), r) for (let e of r) xt(e);
	n && M();
}
function Tt(e) {
	return typeof e == "object" && !!e && typeof e.then == "function";
}
function Et(e) {
	let t = e.o?.Ae;
	t != null && (e.o.Ae = null, t());
}
function Dt(t, n, r) {
	let i = !1, a = !1;
	if (typeof n == "object" && n && qt(() => {
		i = n[Symbol.asyncIterator], a = !i && Tt(n);
	}), !a && !i) return t.o !== null && (t.o.Re = null), t.ge = !1, n;
	V(t).Re = n, t.o.ae = void 0;
	let o = ye, s, c = () => {
		let e = ne(t);
		if (t.o?.Ue && (e = Ie(t) ?? e), e && t.S & 4 && !L(e).oe.has(t)) {
			t.Ge = null;
			return;
		}
		I.initTransition(e), Le(t);
	}, l = (r) => {
		if (t.o?.Re !== n) return;
		let i = r instanceof e;
		if (i && t.ge) {
			t.o !== null && (t.o.Re = null), vt(t, r), t.Pe = T;
			return;
		}
		c(), At(t, i ? 1 : 2, r), i && wt(t), t.Pe = T, i || St(t);
	}, u = (e, i) => {
		if (t.o?.Re !== n || t.ue & 130) return;
		be(o), c();
		let a = !!(t.S & 4), s = t.o?.be;
		kt(t), s && (t.o.be = !0);
		let u = te(t);
		if (u && u.ye.delete(t), r) {
			try {
				r(e);
			} catch (e) {
				l(e);
				return;
			}
			a && kt(t, !0);
		} else if (t.o?.Ce !== void 0 && !(u && t.T & 8388608)) t.ve === m && he(t), t.ve = e, F.me?.(t, e), cn(t) ? F.we(t, e) : xe(t), t.Pe = T;
		else if (u) {
			let n = t.Le, r = cn(t) ? g(t.o.Ce) : t.Qe, i = t.Fe;
			try {
				(!n && a || !i || !i(e, r)) && (n ? t.Qe = e : F.Ve(t, e, u), t.Pe = T, F.me?.(t, e), xe(t, !0));
			} catch (e) {
				At(t, 2, e);
			}
		} else try {
			_n(t, () => e);
		} catch (e) {
			At(t, 2, e);
		}
		t.ve === m && (t.ge = !1, s && (t.o.be = !1), ct(t)), wt(t), M(), je(), i?.();
	}, d = () => t.T & 32 && !t.u && !(t.S & 1) ? (ut(t), !0) : !1, f = (e, r) => {
		let i = e[Symbol.asyncIterator](), a = !1, o = !1, c = !r, f = () => {
			if (!o) {
				o = !0;
				try {
					let e = i.return?.();
					Tt(e) && e.then(void 0, () => {});
				} catch {}
			}
		};
		r ? r(f) : rt(f), V(t).Ae = f;
		let p = () => {
			d() || m();
		}, m = () => {
			let e, r, f = !1, h = !1, g = !0, _ = i.next();
			if ((Tt(_) ? _ : { then: (e) => void e(_) }).then((r) => {
				if (g && c) e = r, f = !0, r.done && (o = !0);
				else if (t.o?.Re !== n) return;
				else r.done ? (o = !0, a ? (M(), je()) : u(void 0), d()) : (a = !0, u(r.value, p));
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
		if (typeof e == "object" && e && qt(() => {
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
			throw I.initTransition(ne(t)), new e(z);
		}
	}
	if (i && h(n), p !== null) {
		if (!p) {
			if (t.ge) return t.Qe;
			throw I.initTransition(ne(t)), new e(z);
		}
		t.ge = !1;
	}
	return s;
}
function Ot(e, t = !1) {
	e.o?.ae && gt(e), e.o?.Ie && e.o !== null && (e.o.Ie = !1), e.o !== null && (e.o.be = !1), e.S = t ? 0 : e.S & 4, e.o?._ && yt(e), (e.o?.je || e.o?.xe) && F.Oe(e), e.o?.i && e.T & 2048 && F.Me !== null && F.Me(e);
	let n = Bt(e);
	n && n.call(e);
}
function kt(e, t = !1) {
	let n = e.o?.ae;
	n && (n.delete(e), n.size) ? (e.o.Ie = !1, t && (e.S = 1), yt(e, n.values().next().value)) : Ot(e, t);
}
function At(n, r, i, a, o) {
	r === 2 && !(i instanceof t) && !(i instanceof e) && (i = new t(n, i));
	let s = r === 1 && i instanceof e ? i.source : void 0, c = s === n, l = r === 1 && n.o?.Ce !== void 0 && !(n.T & 8388608) && !c, u = l && cn(n);
	a || (o && re(n, o), r === 1 && s ? (mt(n, s), n.S & 1 || (n.T &= ~d), n.S = 1 | n.S & 4, yt(n, s, i)) : (gt(n), n.S = r | (r === 2 ? 0 : n.S & 4), V(n)._ = i), F.Oe?.(n), n.o?.i && n.T & 2048 && F.Me !== null && F.Me(n));
	let f = a || u, p = a || l ? void 0 : o, h = Bt(n);
	if (h) {
		if (a && r === 1) return;
		f ? h.call(n, r, i) : h.call(n);
		return;
	}
	bt(n, (t, n) => {
		if (t.Pe = T, r === 1 && n.qe !== t.Ze) {
			Be(t), M();
			return;
		}
		if (r === 1 && s && !t.o?.ae?.has(s) || r !== 1 && (t.o?._ !== i || t.o?.ae)) {
			if (n.He && r !== 1 && !(i instanceof e)) {
				Be(t), M();
				return;
			}
			f || (t.Ge ? s && !t.Le && (t.S & 1 || t.ve !== m) && I.initTransition(t.Ge) : he(t)), At(t, r, i, f, p);
		}
	});
}
F.We = (e) => {
	e.Le === 3 ? (We(e, ze(e)), e.Ye = !0, e.C.enqueue(2, e.Ke)) : Pt(e);
}, F.Be = Xe;
var R = !1, jt = !1, Mt = !1, Nt = !1, z = null, B = null;
function Pt(t, n = !1) {
	ve();
	let r = t.Le;
	if (!n) {
		if (t.Ge && !r && E !== t.Ge && I.initTransition(t.Ge), We(t, ze(t)), t.o !== null && (t.o.Re = null, Et(t)), r === 3 || t.T & 1048576) Xe(t);
		else if (t.Xe !== null || t.ke !== null) {
			Ye(t);
			let e = V(t);
			e.it = t.ke, e.lt = t.Xe, t.ke = null, t.Xe = null, t.ut = 0;
		}
	}
	let o = !!(t.ue & 128), s = !!(t.T & 8388736) && t.o?.Ce !== m && t.o?.Ce !== void 0, c = !!(t.S & 4), l = t.S & 2 ? t.o?._ : void 0, d = !!(t.S & 1), f = d ? t.o?.ae : void 0, p = t.o?.ae?.has(t), h = (t.ue & i) !== 0, _ = t.ge, v = Qt;
	Qt = null;
	let y = z;
	z = t, t.ot = null, t.Ze++, t.ue = 4, t.Pe = T;
	let b = t.ve === m ? t.Qe : t.ve, ee = t.tt, te = !1, ne = R, re = B;
	R = !0;
	let x = Nt;
	if (Nt = !1, r || (B = null), o) {
		let e = F.st(t, !0);
		e ? B = e : e === !1 && (o = !1);
	} else if (t.T & 8388608) {
		let e = F.st(t, !0);
		e && (o = !0, B = e);
	} else if (E && !n && E.rt.length) {
		let e = F.st(t, !1);
		e && (o = !0, B = e);
	}
	let S = r && r !== 2, C = jt;
	S && (jt = !0), r && E !== null && E.ct.size && E.ct.delete(t);
	try {
		if (t.T & 64) b = t.ce(b), t.o !== null && (t.o.Re = null), t.ge = !1;
		else {
			let e = t.o?.Re, n = t.ce(b), r = typeof n == "object" && !!n, i = t.o?.Re !== e;
			b = i || !r ? n : Dt(t, n), !i && !r && (t.o !== null && (t.o.Re = null), t.ge = !1);
		}
		(t.S !== 0 || t.o !== null) && Ot(t, n && Qt === null), t.T & 1024 && t.o?.Ue && F.ft(t);
	} catch (n) {
		let r = n instanceof e;
		if (r && t.ge) vt(t, n);
		else {
			r && B && F._t(t);
			let e = !1;
			if (r && (V(t).Ie = !0, F.Nt !== null && (e = F.Nt(t, h))), At(t, r ? 1 : 2, n, void 0, r ? t.o?.Ue : void 0), r && p && !t.o?.Re && wt(t), r && f) for (let e of f) e !== t && !t.o?.ae?.has(e) && wt(t, e);
			e && F.k(t);
		}
	} finally {
		R = ne, Nt = x, S && (jt = C), te = (t.ue & a) !== 0, t.ue = 0 | (n ? t.ue & 256 : 0), z = y;
	}
	let w = Qt;
	if (Qt = v, !t.o?._) {
		let e = s ? g(t.o?.Ce) : o || t.ve === m ? t.Qe : t.ve, i = !1;
		try {
			i = !r && c || !t.Fe || !t.Fe(e, b);
		} catch (e) {
			At(t, 2, e);
		}
		if (r && i && (t.Ye = !t.o?._, !n)) {
			t.C.enqueue(r, t.dt ??= F.Et.bind(null, t));
			let e = t.It;
			e !== E && (t.It = E, e !== null && (e = L(e)) !== E && !e.Tt && ((e.St ??= []).push(t), E !== null && (E.St ??= []).push(t)));
		}
		if (!t.o?._) {
			if (i) {
				let e = s ? t.o?.Ce : void 0;
				n && w === null || r && w === null && (E !== t.Ge || E === null || t.T & 32768) || o ? (o && !r && B !== null ? F.Ve(t, b, B) : t.Qe = b, o && (t.ve = m)) : (t.ve = b, w !== null && (t.Ge = w, w.Ot.push(t), r && w.ct.add(t)), _ && (t.ge = !0), t.T & 256 && F.me !== null && F.me(t, b)), t.u !== null && (!s || o || t.o?.Ce !== e) ? xe(t, o || s) : s && !o && t.o.Ct !== T && F.we(t, b);
			} else if (s) t.ve === m && he(t), t.ve = b, _ && (t.ge = !0), F.we(t, b);
			else if (t.tt != ee) for (let e = t.u; e !== null; e = e.Ne) Ue(e._e, ze(e._e));
		}
		if (!i && !t.o?._ && (l !== void 0 && Ct(t, l), f)) for (let e of f) e !== t && wt(t, e);
		p && !(t.S & 5) && wt(t);
	}
	let D = t.ot;
	r && (d && !(t.S & 1) || (D === null ? t.Se !== null : D.de !== null)) && le(), !t.o?._ && t.ve === m && !(r && t.Ye) && (n || o || r === 3 ? ct(t) : (t.ot?.de ?? t.Se) && Te.push(t)), B = re;
	let O = (t.ve !== m || t.o !== null && (t.o.lt !== null || t.o.it !== null) || !!(t.S & 5)) && (!n || w !== null || !!(t.S & 1));
	if (O && (!t.Ge || s) ? he(t) : O && E === null && !(t.S & 5) && (O = !1, Xe(t, !1, !0)), O ? t.T |= u : t.T &= ~u, t.Ge && r && E !== t.Ge && w === null) {
		let e = t.It;
		Re(t.Ge, () => Pt(t)), t.It = e;
	}
	te && (Be(t), M());
}
function Ft(e) {
	if (!(e.ue & 68)) {
		if (e.ue & 1) for (let t = e.Se; t; t = t.de) {
			let n = t.Ee, r = n.Te || n;
			if (r.ce && Ft(r), e.ue & 2) break;
		}
		(e.ue & 130 || e.o?._ && e.Pe < T && !e.o?.Re) && Pt(e), e.ue &= 280;
	}
}
function It(e, t) {
	let n = t?.transparent ?? !1, r = typeof t == "object" && !!t && "loadingValue" in t, i = {
		id: et(t, n, z),
		T: (n ? 4 : 0) | !!t?.ownedWrite | (!z || t?.lazy ? 32 : 0) | (t?.sync ? 64 : 0) | (t?.Z ? 2 : 0) | 0,
		Fe: t?.equals ?? Kt,
		ke: null,
		C: z?.C ?? I,
		ze: z?.ze ?? _,
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
		_parent: z,
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
	return t?.unobserved && (V(i).Pt = t.unobserved), Ht(i, t), i;
}
function V(e) {
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
function Lt(e, t, n, r, i) {
	let a = i?.transparent ?? !1, o = {
		id: et(i, a, z),
		T: (a ? 4 : 0) | !!i?.ownedWrite | (i?.sync ? 64 : 0) | (i?.kt ?? 0) | 0,
		Fe: !1,
		ke: null,
		C: z?.C ?? I,
		ze: z?.ze ?? _,
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
		_parent: z,
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
	return i?.unobserved && (V(o).Pt = i.unobserved), Ht(o, Vt), o;
}
var Rt = null;
function zt(e) {
	Rt = e;
}
function Bt(e) {
	let t = e.o?.h;
	return t === void 0 ? e.Le ? Rt ?? void 0 : void 0 : t;
}
var Vt = { lazy: !0 };
function Ht(e, t) {
	e.Rt = e;
	let n = z?.xt ? z.Qt : z;
	if (z) {
		let t = z.Xe;
		t === null ? z.Xe = e : (e.$e = t, t.Dt = e, z.Xe = e);
	}
	n && (e.tt = n.tt + 1), F.wt !== null && F.wt(e), !t?.lazy && Pt(e, !0);
}
function Ut(e, t, n = null) {
	let r = {
		Fe: t?.equals ?? Kt,
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
	return t?.unobserved && (V(r).Pt = t.unobserved), n && Gt(n, r), r;
}
var Wt;
function Gt(e, t) {
	let n = t.De;
	n !== null && (n.Mt = t), V(e).i = t, e.T |= s;
}
function Kt(e, t) {
	return e === t;
}
function qt(e, t) {
	if (F.Yt === null && !R) return e();
	let n = R;
	R = !1;
	try {
		return F.Yt === null ? e() : F.Yt(e);
	} finally {
		R = n;
	}
}
function Jt(e, t) {
	e.ue & 512 ? (e.ue &= -513, Pt(e, !0)) : e.ue & 64 ? e.T & 32 && Pt(e, !0) : t && Ft(e);
}
function Yt(e, t) {
	let n = t.It;
	(n == null || L(n) !== e) && e.ct.add(t);
}
function Xt(e) {
	return E !== null && L(e) === L(E);
}
function Zt(e, t) {
	let n = e.Ge;
	if (n === null || Xt(n)) return !1;
	let r = L(n);
	Yt(r, t);
	let i = r.oe.get(e);
	return i ? i.add(t) : e.S & 1 && Re(r, () => t.C.notify(t, 1, 1, e.o._)), !0;
}
var Qt = null;
function $t(e, t = e.Ge) {
	if (!t || t === E || e?.o?.Ht || z?.o?.Ht) return;
	let n = z;
	if (E === null && !I.Kt) {
		if (F.jt) return;
		if (n.ue & 4 && !(n.T & 128) && (Qt === null || Qt === t)) {
			Qt = t;
			return;
		}
	}
	I.initTransition(t);
}
function en(e, t, n, r) {
	return !!(!t || B !== null && F.Bt(e, n, t) || e.ve === m || t.T & 16 || jt && !r && Zt(e, t) || e.T & 131072 && !Nt && !(t.T & 8192));
}
var tn = !1;
function nn() {
	tn = !0;
}
function rn(e, t = e.Qe) {
	return I.Kt || e.ve === m || e.T & 4194304 || e.o?.Ht ? m : e.Ge === null || e.T & 16777216 ? t : e.o === null ? m : e.o.gt;
}
var an = [], on = [];
function sn(e) {
	return !I.Kt && e.o?.Ct === T && !e.o?.Ht;
}
function cn(e) {
	let t = e.o;
	return t !== null && t.Ce !== void 0 && t.Ce !== m;
}
function ln(e) {
	return cn(e) && !sn(e);
}
function un(e) {
	return e.ue |= a, !0;
}
var dn = [];
function fn() {
	if (tn = !1, an.length !== 0) {
		for (let e of an) e.o.gt = m;
		an.length = 0;
	}
	if (on.length !== 0) {
		for (let e of on) e.T &= ~f;
		on.length = 0;
	}
	if (dn.length !== 0) {
		for (let e of dn) F.me(e, e.ve === m ? e.Qe : e.ve);
		dn.length = 0;
	}
}
function pn(e) {
	if (Nt) return F.zt(e);
	let t = z;
	t?.xt && (t = t.Qt);
	let n = e, r = e.Te || e;
	if (typeof n.ce == "function" && Jt(e, !1), !n.ce && r === e && e.o?.Ce === void 0 && e.o?.nt === void 0 && E === null && B === null && (!tn || e.ve === m)) return t && R && pt(e, t), !t || e.ve === m || t.T & 16 || jt && Zt(e, t) ? e.Qe : ($t(e), e.ve);
	if (t && R && (pt(e, t, Mt), r.ce)) {
		let n = ze(e);
		r.tt >= n.et ? (Ke(t), Ge(n), Ft(r)) : t.T & 65536 && Ft(r);
		let i = r.tt;
		i >= t.tt && e._parent !== t && (t.tt = i + 1);
	}
	if (r.S & 1) {
		if (t && (!jt || r.S & 4 || r.T & 2097152 || r.T & 1024 && F.Xt(r) || !Zt(r, t))) {
			if (B === null || F.$t(r)) throw !R && e !== t && pt(e, t), r.o?._;
		} else if (!t && r.S & 4) throw r.o?._;
	}
	if (r.ce && r.S & 2) {
		if (R && r.Pe < T) return Pt(r), pn(e);
		throw r.o?._;
	}
	let i = mn(e, t, r, e.Qe);
	return !t && r === e && typeof n.ce == "function" && e.T & 32 && !(r.S & 1) && !e.u && !ln(e) && (dt.add(e), M()), i;
}
function mn(t, n, r, i) {
	if (cn(t)) {
		if (!(n && n.T & 8192) && !sn(t)) return n && t.T & 525312 ? F.en(t, n) : g(t.o?.Ce);
		t.T |= c;
	}
	if (B !== null && E !== null && n !== null && F.tn(t, r, n)) return i;
	let a = t.ve !== m && !!(t.S & 4);
	if (a && !n) throw new e(null);
	let o = n && tn ? rn(t, i) : m;
	return o === m ? en(t, n, r, a) ? i : ($t(t), t.ve) : (un(n), o);
}
function hn(e) {
	if (I.Kt) return;
	let t = V(e);
	t.gt === m && (t.gt = e.ve, an.push(e), tn = !0);
}
function gn(e) {
	I.Kt || e.T & 4194304 || (e.T |= f, on.push(e));
}
function _n(e, t) {
	if (e.Ge && E !== e.Ge && (I.Kt ? I.initTransition(e.Ge) : (P.push(e.Ge), M())), e.T & 128) return F.ln(e, t);
	let n = e.ve === m ? e.Qe : e.ve;
	if (typeof t == "function" && (t = t(n)), !(e.S & 4 || !e.Fe || !e.Fe(n, t))) return t;
	let r = e.ve !== m;
	return r ? e.Ge !== null && hn(e) : he(e), e.ve = t, z !== null && gn(e), e.T & 256 && F.me !== null && (F.me(e, t), I.Kt || dn.push(e)), e.ce !== void 0 && (e.Pe = T), r && e.ht === _e && B === null ? t : (xe(e), M(), t);
}
function vn(e) {
	We(e, ze(e)), !(e.ue & 1024) && e.ve === m && (he(e), M()), e.ue = e.ue & -4 | r, e.sn = T;
}
function yn(e, t) {
	let n = _n(e, t);
	return vn(e), n;
}
function bn(e, t) {
	let n = z, r = R;
	z = e, R = !1;
	try {
		return t();
	} finally {
		z = n, R = r;
	}
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/core/effect.js
function xn(e, t, n, r) {
	let i = Lt(e, t, n, r?.user ? 2 : 1, r);
	Pt(i, !0), !r?.defer && i.ve === m && (i.Le === 2 || r?.schedule ? i.C.enqueue(i.Le, Cn.bind(null, i)) : Cn(i, 4));
}
function Sn(e, t) {
	let r = e === void 0 ? this.S : e, i = t === void 0 ? this.o?._ : t;
	if (r & 2) {
		if (this.C.notify(this, 1, 0), this.Le === 2) {
			this.S & 2 && (this.Ye = !0, this.C.enqueue(this.Le, this.dt ??= Cn.bind(null, this)));
			return;
		}
		if (!this.C.notify(this, 2, 2)) throw de(n(i)), i;
	} else this.Le === 1 && this.C.notify(this, 3, r, i);
}
function Cn(e, r) {
	if (!e.Ye || e.ue & 64) return;
	if (e.It !== null && !L(e.It).Tt && (r & 4 ? !e.o?.Ue : E !== null)) {
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
			if (!e.C.notify(e, 2, 2)) throw de(t), t;
		}
		return;
	}
	let i = e.o?._ == null, a = e.yt;
	e.yt = void 0;
	try {
		a?.(), e.yt = e.Lt(e.Qe, e.Ut);
	} catch (n) {
		if (V(e)._ = new t(e, n), e.S |= 2, !e.C.notify(e, 2, 2)) throw de(n), n;
	} finally {
		e.Ut = e.Qe, e.Ye = !1, i && ct(e);
	}
}
F.Et = Cn, zt(Sn);
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/signals.js
function wn(e) {
	return rt(e);
}
function Tn(e) {
	let t = pn.bind(null, e);
	return t[v] = e, t;
}
function En(e, t) {
	if (typeof e == "function") {
		let n = It(e, t);
		return n.T &= -33, [Tn(n), yn.bind(null, n)];
	}
	let n = Ut(e, t);
	return [Tn(n), _n.bind(null, n)];
}
function Dn(e, t) {
	return Tn(It(e, t));
}
function On(e, t, n) {
	xn(e, t, void 0, n);
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/store/store.js
var kn = Symbol(0), An = Symbol(0);
function jn(e) {
	return Reflect.ownKeys(e).filter((t) => Object.prototype.propertyIsEnumerable.call(e, t));
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/map.js
function Mn(e, t, n) {
	let r = typeof n?.keyed == "function" ? n.keyed : void 0, i = t.length > 1, a = t, o = {
		se: at(),
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
	}, s = It(In.bind(o), void 0);
	return o.se.Qt = s, s.T &= -33, Tn(s);
}
var Nn = { ownedWrite: !0 };
function Pn(e, t, n, r) {
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
	return Fn(e, t, n, o, s, c);
}
function Fn(e, t, n, r, i, a) {
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
function In() {
	let e = this.ss() || [], t = e.length;
	return e[kn], bn(this.se, () => {
		let n, r, i, a, o = this.fs ? this.ls ? () => (i[r] = Ut(e[r], Nn), this.rs(Tn(i[r]), r)) : () => (i[r] = Ut(e[r], Nn), a && (a[r] = Ut(r, Nn)), this.rs(Tn(i[r]), a ? Tn(a[r]) : void 0)) : this.cs ? () => {
			let t = e[r];
			return a[r] = Ut(r, Nn), this.rs(t, Tn(a[r]));
		} : () => {
			let t = e[r];
			return this.rs(t);
		};
		if (t === 0) this.ts !== 0 && (this.se.dispose(!1), this.hs = [], this.es = [], this.ns = [], this.ts = 0, this.fs &&= [], this.cs &&= []), this.us && !this.ns[0] && (this.hs[0]?.dispose(), this.ns[0] = bn(this.hs[0] = at(), this.us));
		else if (this.ts === 0) {
			let s = Array(t), c = Array(t);
			i = this.fs && Array(t), a = this.cs && Array(t);
			try {
				for (r = 0; r < t; r++) s[r] = bn(c[r] = at(), o);
			} catch (e) {
				for (n = 0; n <= r; n++) c[n]?.dispose();
				throw e;
			}
			this.hs[0] && this.hs[0].dispose(), this.ns = s, this.hs = c, i && (this.fs = i), a && (this.cs = a), this.es = e.slice(0), this.ts = t;
		} else {
			let s, c, l, u, d, f, p, m, h;
			for (s = 0, c = Math.min(this.ts, t); s < c && (this.es[s] === e[s] || this.fs && Ln(this.qt, this.es[s], e[s])); s++) this.fs && _n(this.fs[s], e[s]);
			for (c = this.ts - 1, l = t - 1; c >= s && l >= s && (this.es[c] === e[l] || this.fs && Ln(this.qt, this.es[c], e[l])); c--, l--);
			if (s === t && this.ts === t) {
				this.es = e.slice(0);
				return;
			}
			if (t <= this.ts && c - s > 64 && this.fs === void 0 && this.cs === void 0) {
				let n = s + (l - s >> 1), r = e[n], i = Math.min(c, n + 32), a = Math.max(s, n - 32);
				for (; a <= i && this.es[a] !== r;) a++;
				if (a <= i && Pn(this, e, t, s)) return;
			}
			let g = t - this.ts, _ = Array(t), v = Array(t);
			for (i = this.fs ? Array(t) : void 0, a = this.cs ? Array(t) : void 0, f = /* @__PURE__ */ new Map(), p = Array(l + 1), r = l; r >= s; r--) u = e[r], d = this.qt ? this.qt(u) : u, n = f.get(d), p[r] = n === void 0 ? -1 : n, f.set(d, r);
			for (n = s; n <= c; n++) u = this.es[n], d = this.qt ? this.qt(u) : u, r = f.get(d), r !== void 0 && r !== -1 ? (_[r] = this.ns[n], v[r] = this.hs[n], i && (i[r] = this.fs[n]), a && (a[r] = this.cs[n]), r = p[r], f.set(d, r)) : (m ??= []).push(this.hs[n]);
			try {
				for (r = s; r <= l; r++) v[r] === void 0 && ((h ??= []).push(v[r] = at()), _[r] = bn(v[r], o));
			} catch (e) {
				if (h) for (n = 0; n < h.length; n++) h[n].dispose();
				throw e;
			}
			for (n = 0; n < s; n++) _[n] = this.ns[n], v[n] = this.hs[n], i && (i[n] = this.fs[n]), a && (a[n] = this.cs[n]);
			for (r = s; r <= l; r++) i && _n(i[r], e[r]), a && _n(a[r], r);
			for (r = l + 1; r < t; r++) _[r] = this.ns[r - g], v[r] = this.hs[r - g], i && (i[r] = this.fs[r - g], _n(i[r], e[r])), a && (a[r] = this.cs[r - g], g !== 0 && _n(a[r], r));
			if (this.ns = _, this.hs = v, i && (this.fs = i), a && (this.cs = a), this.ts = t, this.es = e.slice(0), m) for (n = 0; n < m.length; n++) m[n].dispose();
		}
	}), this.ns;
}
function Ln(e, t, n) {
	return !e || e(t) === e(n);
}
//#endregion
//#region node_modules/.pnpm/@solidjs+signals@2.0.0-rc.9/node_modules/@solidjs/signals/dist/prod/boundaries.js
function Rn(e, t) {
	if (typeof e == "function" && !e.length) {
		if (t?.doNotUnwrap) return e;
		do
			e = e();
		while (typeof e == "function" && !e.length);
	}
	if (!t?.skipNonRendered || e != null && e !== !0 && e !== !1 && e !== "") {
		if (Array.isArray(e)) {
			let n = [];
			return zn(e, n, t) ? () => {
				let e = [];
				return zn(n, e, {
					...t,
					doNotUnwrap: !1
				}), e;
			} : n;
		}
		return e;
	}
}
function zn(t, n = [], r) {
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
		Array.isArray(e) ? a = zn(e, n, r) || a : r?.skipNonRendered && (e == null || e === !0 || e === !1 || e === "") || n.push(e);
	} catch (t) {
		if (!(t instanceof e)) throw t;
		i = t;
	}
	if (i) throw i;
	return a;
}
var Bn = Object.freeze({});
function Vn(e, t) {
	return t === 3 ? (e = e()) ?? Bn : e;
}
function Hn(e, t) {
	let n = e.hidden;
	return typeof n == "function" ? n(t) : n.includes(t);
}
function Un(e) {
	return Vn(e.source, e.kind);
}
function Wn(e, t) {
	return t === 0 ? Object.keys(e) : t === 2 || e[An] === e ? Reflect.ownKeys(e) : Object.keys(e);
}
function Gn(e, t) {
	if (t === 1) {
		if (e.kind === 4) return qn(e.source, !1, e);
		let t = Wn(Un(e), e.kind), n = [];
		for (let r = 0; r < t.length; r++) Hn(e, t[r]) || n.push(t[r]);
		return n;
	}
	return Wn(Vn(e, t), t);
}
function Kn(e, t) {
	if (e !== void 0) {
		for (let n = e.length - 1; n >= 0; n--) if (Hn(e[n], t)) return !0;
	}
	return !1;
}
function qn(e, t, n) {
	let r = [];
	return Jn(e, n === void 0 ? void 0 : [n], t, r, null), r;
}
function Jn(e, t, n, r, i) {
	let a = e.sources, o = e.kinds;
	for (let e = 0; e < a.length; e++) {
		let s = a[e], c = o[e], l;
		if (c === 1) {
			if (s.kind === 4) {
				t === void 0 ? t = [s] : t.push(s), Jn(s.source, t, n, r, i), t.pop();
				continue;
			}
			l = s, c = s.kind, s = s.source;
		}
		s = Vn(s, c);
		let u = n ? jn(s) : Wn(s, c);
		for (let e = 0; e < u.length; e++) {
			let n = u[e];
			l !== void 0 && Hn(l, n) || Kn(t, n) || Yn(r, i, n, s);
		}
	}
}
function Yn(e, t, n, r) {
	let i = e.indexOf(n);
	i !== -1 && (e.splice(i, 1), t !== null && t.splice(i, 1)), e.push(n), t !== null && t.push(r);
}
//#endregion
//#region node_modules/.pnpm/solid-js@2.0.0-rc.9/node_modules/solid-js/dist/solid.js
var Xn = !1, Zn = {
	hydrating: !1,
	registry: void 0,
	done: !1
}, Qn = (...e) => Dn(...e), H = (...e) => En(...e), $n = (...e) => ot(...e), er = (...e) => On(...e);
function U(e, t, n) {
	return qt(() => e(t || {}));
}
var tr = (e) => `Stale read from <${e}>.`;
function nr(e) {
	let t = "fallback" in e ? {
		keyed: e.keyed,
		fallback: () => e.fallback
	} : { keyed: e.keyed }, n = nt(), r, i = () => bn(n, () => Mn(() => e.each, e.children, t));
	return Zn.hydrating && (r = i()), () => (r ??= i())();
}
function W(e) {
	let t = e.keyed, n = Dn(() => e.when, void 0), r = t ? n : Dn(n, {
		equals: (e, t) => !e == !t,
		sync: !0
	});
	return Dn(() => {
		let i = r();
		if (i) {
			let a = e.children;
			return typeof a == "function" && a.length > 0 ? qt(t ? () => a(i) : () => a(() => {
				if (!qt(r)) throw tr("Show");
				return n();
			}), Xn) : a;
		}
		return e.fallback;
	}, { sync: !0 });
}
//#endregion
//#region node_modules/.pnpm/@solidjs+web@2.0.0-rc.9_solid-js@2.0.0-rc.9/node_modules/@solidjs/web/dist/web.js
var G = /*#__PURE__*/ Symbol("slot"), rr = /*#__PURE__*/ Symbol("host"), ir = {
	transparent: !0,
	sync: !0
}, ar = { sync: !0 };
function K(e, t, n) {
	er(e, t, n ? {
		sync: !0,
		...n,
		transparent: !n.scope
	} : ir);
}
function or(e) {
	return Qn(() => e(), ar);
}
function sr(e, t, n, r) {
	let i = n.length, a = t.length, o = i, s = 0, c = 0, l = t[a - 1], u = l[G], d = l.parentNode === e && (!u || u === r) ? l.nextSibling : r || null, f = null, p, m, h = (t) => {
		if (!t) return !1;
		let n = t[G];
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
					let i = n[c - 1], a = i[G];
					t = i.parentNode === e && (!a || a === r) ? i.nextSibling : d;
				} else t = n[o - c];
			} else t = d;
			for (; c < o;) {
				let i = n[c++];
				e.insertBefore(i, t), r && (i[G] = r);
			}
		} else if (o === c) for (; s < a;) {
			let n = t[s++];
			if (!f || !f.has(n)) {
				let t = n[G];
				n.parentNode === e && (!t || t === r) && n.remove();
			}
		}
		else if ((p = t[s]) === n[o - 1] && n[c] === t[a - 1] && p.parentNode === e && (!(m = p[G]) || m === r)) {
			if (r) do {
				let n = t[--a];
				if (e.insertBefore(n, p), n[G] = r, c++, s >= a - 1 || c >= o) break;
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
						let a = t[s], o = a[G], l = a.parentNode === e && (!o || o === r) ? a : d;
						for (; c < i;) {
							let t = n[c++];
							e.insertBefore(t, l), r && (t[G] = r);
						}
					} else {
						let i = t[s++], a = n[c++], o = i[G];
						i.parentNode === e && (!o || o === r) ? e.replaceChild(a, i) : e.insertBefore(a, d), r && (a[G] = r);
					}
				} else s++;
			} else {
				let n = t[s++], i = n[G];
				n.parentNode === e && (!i || i === r) && n.remove();
			}
		}
	}
}
var cr = "_$$", lr = "_$SOLID_EVENT_OWNER", ur = {}, dr = /* @__PURE__ */ new Set(), fr = /* @__PURE__ */ new Map();
function pr(e, t, n, r = {}) {
	let i;
	gr(t);
	try {
		$n((a) => {
			if (i = a, r.onError && (nt()[ue] = r.onError), t === document) {
				let t = e();
				K(() => Rn(t), () => {});
			} else {
				let i = e();
				X(t, () => i, t.firstChild ? null : void 0, n, {
					...r.insertOptions,
					schedule: !0
				});
			}
		}, { id: r.renderId }), je();
	} catch (e) {
		throw i && i(), _r(t), e;
	}
	return () => {
		i(), _r(t), t.textContent = "";
	};
}
function mr(e, t, n) {
	let r = document.createElement("template");
	return r.innerHTML = e, n === 2 ? r.content.firstChild.firstChild : r.content.firstChild;
}
function q(e, t) {
	let n;
	return t === 1 ? (r) => document.importNode(n ||= mr(e, r, t), !0) : (r) => (n ||= mr(e, r, t)).cloneNode(!0);
}
function hr(e) {
	for (let t = 0, n = e.length; t < n; t++) {
		let n = e[t];
		dr.has(n) || (dr.add(n), fr.forEach((e, t) => br(n, t, e)));
	}
}
function gr(e) {
	let t = vr(e, e);
	t && (t.roots = (t.roots || 0) + 1);
}
function _r(e) {
	let t = fr.get(e);
	t && (t.roots > 1 ? t.roots-- : delete t.roots), yr(e, e);
}
function vr(e, t = e) {
	if (!e || !t) return;
	let n = fr.get(e);
	return n || fr.set(e, n = {
		owners: /* @__PURE__ */ new Map(),
		handlers: /* @__PURE__ */ new Map()
	}), n.owners.set(t, (n.owners.get(t) || 0) + 1), dr.forEach((t) => br(t, e, n)), n;
}
function yr(e, t = e) {
	let n = fr.get(e);
	if (!n) return;
	let r = n.owners.get(t);
	r > 1 ? n.owners.set(t, r - 1) : n.owners.delete(t), !n.owners.size && (n.handlers.forEach((t, n) => e.removeEventListener(n, t)), fr.delete(e));
}
function br(e, t, n) {
	if (n.handlers.has(e)) return;
	let r = (e) => Nr(e, t, n);
	n.handlers.set(e, r), t.addEventListener(e, r);
}
function xr(e, t) {
	let n = e, r = 0;
	for (; n;) {
		if (t.owners.has(n)) return {
			owner: n,
			distance: r
		};
		r++, n = n._$host || n.parentNode || n.host;
	}
}
var Sr = null;
function Cr(e) {
	if (Sr !== null) for (let t = 0; t < Sr.length; t++) Sr[t](e);
	return e;
}
function J(e, t, n) {
	if (Ar(e)) return;
	let r = t === "multiple" && e.localName === "select";
	if (n == null || n === !1) e.removeAttribute(t);
	else if (e.setAttribute(t, n === !0 ? "" : n), r && !e._$multiple) {
		let t = e.options;
		for (let e = 0; e < t.length; e++) t[e].defaultSelected && (t[e].selected = !0);
	}
	r && (e._$multiple = !0), Sr !== null && (t === "href" || t === "action") && Cr(e);
}
function Y(e, t, n) {
	if (typeof t == "number" && (t = "" + t), typeof n == "number" && (n = "" + n), Ar(e)) {
		e._$classes = t && typeof t == "object" ? jr(t) : void 0;
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
	typeof n == "string" ? (r = {}, e.removeAttribute("class")) : r = e._$classes || jr(n || {}), t = jr(t);
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
function wr(e) {
	if (typeof e != "object" || !e) return e;
	if (Array.isArray(e)) return e.map(wr);
	if (e[An] !== e) return e;
	let t = Gn(e, 2), n = {};
	for (let r = 0; r < t.length; r++) {
		let i = t[r];
		typeof i == "string" && (n[i] = e[i]);
	}
	return n;
}
function Tr(e, t, n) {
	Ar(e) || (n == null ? e.style.removeProperty(t) : e.style.setProperty(t, n));
}
function Er(e, t) {
	Array.isArray(e) ? e.flat(Infinity).forEach((e) => e && e(t)) : e(t);
}
function Dr(e, t) {
	let n = qt(e);
	bn(null, () => Er(n, t));
}
var Or = { scope: !0 }, kr = null;
function X(e, t, n, r, i) {
	let a = n !== void 0, o = i && i.host;
	if (a && !r && (r = []), kr !== null && (r = kr.claimInitial(e, a, r)), typeof t != "function" && (t = Fr(t, r, a, !0), typeof t != "function")) {
		Pr(e, t, r, n), o && Ir(t, o);
		return;
	}
	if (a && r.length === 0) {
		let t = document.createTextNode("");
		e.insertBefore(t, n), r = [t];
	}
	let s = r;
	K((r) => {
		kr !== null && (s = kr.reclaimRegion(s, e, n));
		let c = Fr(t(), s, a, !0);
		return typeof c == "function" ? (K(() => (kr !== null && (s = kr.reclaimRegion(s, e, n)), Fr(c, s, a)), (t) => {
			s = Pr(e, t, s, n), o && Ir(s, o);
		}, r !== void 0 && !(i && i.schedule) ? {
			...i,
			schedule: !0
		} : i), ur) : c;
	}, (t) => {
		t !== ur && (s = Pr(e, t, s, n), o && Ir(s, o));
	}, t.$s ? i ? {
		...i,
		scope: !0
	} : Or : i);
}
function Ar(e) {
	if (!Zn.hydrating || Zn.isClaiming && !Zn.isClaiming()) return !1;
	if (!e || e.isConnected) return !0;
	let t = Zn.claimRoots;
	if (t) {
		for (let n = 0; n < t.length; n++) if (t[n].contains(e)) return !0;
	}
	return !1;
}
function jr(e) {
	if (Array.isArray(e)) {
		let t = {};
		Mr(e, t), e = t;
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
function Mr(e, t) {
	for (let n = 0, r = e.length; n < r; n++) {
		let r = e[n];
		Array.isArray(r) ? Mr(r, t) : typeof r == "object" && r ? Object.assign(t, r) : typeof r != "boolean" && (r || r === 0) && (t[r] = !0);
	}
}
function Nr(e, t, n) {
	if (kr !== null && kr.dedupEvent(e)) return;
	let r = e[lr], i;
	if (r) {
		if (r === !0 || r === t || !t.contains(r)) return;
		i = r;
	}
	let a = n && (n.owners.size === 1 && n.owners.has(t) ? t : xr(e.target, n)?.owner);
	if (n && !a || a && a === i) return;
	e[lr] = a || !0;
	let o = i || e.target, s = cr + e.type, c = e.target, l = a || t || e.currentTarget, u = (t) => Object.defineProperty(e, "target", {
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
function Pr(e, t, n, r) {
	if (kr !== null && Ar(e)) {
		if (t && t !== n) {
			let e = Array.isArray(t);
			for (let r of e ? t : [t]) if (r && r.nodeType) {
				if (!Ar(r)) return n;
			} else if (e && (typeof r == "string" || typeof r == "number")) return n;
		}
		return t;
	}
	if (t === n) return t;
	let i = typeof t, a = r !== void 0;
	if (i === "string" || i === "number") {
		let r = typeof n;
		r === "string" || r === "number" ? e.firstChild.data = t : Rr(e, n) ? e.textContent = t : (zr(e, n), e.insertBefore(document.createTextNode(t), e.firstChild));
	} else if (t === void 0) Br(e, n, r);
	else if (t.nodeType) Array.isArray(n) ? Br(e, n, a ? r : null, t) : n && n.nodeType ? n.parentNode === e ? e.replaceChild(t, n) : e.appendChild(t) : n && e.firstChild ? e.replaceChild(t, e.firstChild) : e.appendChild(t), r && (t[G] = r);
	else if (Array.isArray(t)) {
		let i = n && Array.isArray(n);
		for (let e = 0, r = t.length; e < r; e++) {
			let r = t[e], a = typeof r;
			if (a === "string" || a === "number") {
				let a = i ? n[e] : void 0;
				a && a.nodeType === 3 ? (a.data !== "" + r && (a.data = r), t[e] = a) : t[e] = document.createTextNode(r);
			}
		}
		t.length === 0 ? Br(e, n, r) : i ? n.length === 0 ? Lr(e, t, r) : sr(e, n, t, r) : (n && Br(e, n), Lr(e, t));
	}
	return t;
}
function Fr(e, t, n, r) {
	if (e = Rn(e, {
		skipNonRendered: !0,
		doNotUnwrap: r
	}), r && typeof e == "function") return e;
	if (n && !Array.isArray(e) && (e = [e ?? ""]), Zn.hydrating && Array.isArray(e)) for (let n = 0, r = e.length; n < r; n++) {
		let r = e[n], i = t && t[n], a = typeof r;
		(a === "string" || a === "number") && i && i.nodeType === 3 && Ar(i) && (e[n] = i);
	}
	return e;
}
function Ir(e, t) {
	if (Array.isArray(e)) for (let n = 0, r = e.length; n < r; n++) Ir(e[n], t);
	else e && e.nodeType && e[rr] !== t && (e[rr] = t, Object.defineProperty(e, "_$host", {
		get: t,
		configurable: !0
	}));
}
function Lr(e, t, n = null) {
	for (let r = 0, i = t.length; r < i; r++) {
		let i = t[r];
		e.insertBefore(i, n), n && (i[G] = n);
	}
}
function Rr(e, t) {
	if (t == null) return !0;
	if (Array.isArray(t)) return t.length ? e.firstChild === t[0] && e.lastChild === t[t.length - 1] : e.firstChild === null;
	if (t === "") return e.firstChild === null;
	if (t.nodeType) return e.firstChild === t && e.lastChild === t;
	let n = e.firstChild;
	return n !== null && n.nodeType === 3 && e.lastChild === n;
}
function zr(e, t) {
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
function Br(e, t, n, r) {
	if (n === void 0) return Rr(e, t) ? e.textContent = "" : zr(e, t);
	if (t.length) {
		let i = !1;
		for (let a = t.length - 1; a >= 0; a--) {
			let o = t[a];
			if (r !== o) {
				let t = o[G], s = o.parentNode === e && (!t || t === n);
				r && !i && !a ? s ? e.replaceChild(r, o) : e.insertBefore(r, n) : s && o.remove();
			} else i = !0;
		}
	} else r && e.insertBefore(r, n);
	r && n && (r[G] = n);
}
//#endregion
//#region src/status.tsx
var Vr = /* @__PURE__ */ q("<svg class=pos-symbol-defs aria-hidden=true><defs><clipPath id=pos-coin-fragment><path d=\"M0 0h21.5l-3.5 9.5 3.5 5.9L17.8 32H0Z\"></path></clipPath><symbol id=pos-coin viewBox=\"0 0 32 32\"><path d=\"M4 12.5v6c0 5 5.4 9 12 9s12-4 12-9v-6\"fill=currentColor fill-opacity=.24 stroke=currentColor stroke-width=1.6 stroke-linejoin=round></path><path d=\"M8 22.5v3M16 24.5v3M24 22.5v3\"fill=none stroke=currentColor stroke-opacity=.55 stroke-width=1></path><ellipse cx=16 cy=12.5 rx=12 ry=9 fill=var(--pos-coin-face) stroke=currentColor stroke-width=1.6></ellipse><path d=\"M9.5 15.7V9.4l6.5 5 6.5-5v6.3\"fill=none stroke=#ff6600 stroke-width=2.5 stroke-linecap=square></path><path d=\"M9.5 15.7v1.2h13v-1.2\"fill=none stroke=currentColor stroke-width=1></path></symbol><symbol id=pos-coin-partial viewBox=\"0 0 32 32\"><use href=#pos-coin clip-path=url(#pos-coin-fragment)></use><path d=\"M21.5 4 18 9.5 21.5 15.4 17.8 27\"fill=none stroke=currentColor stroke-width=1.2 stroke-linejoin=round></path><path d=\"M21.5 4.2C25.6 6 28 8.9 28 12.5v6c0 4.7-4.2 8.1-10.2 8.5\"fill=none stroke=currentColor stroke-width=1.4 stroke-dasharray=\"2.2 2.2\"stroke-linecap=round></path></symbol><symbol id=pos-coins-overpaid viewBox=\"0 0 44 32\"><use href=#pos-coin x=0 y=3 width=30 height=29></use><use href=#pos-coin x=13 y=0 width=30 height=29>"), Hr = /* @__PURE__ */ q("<span class=pos-spinner>"), Ur = /* @__PURE__ */ q("<span class=pos-disc>"), Wr = /* @__PURE__ */ q("<svg viewBox=\"0 0 32 32\"><use href=#pos-coin-partial>"), Gr = /* @__PURE__ */ q("<svg viewBox=\"0 0 32 32\"><use href=#pos-coin>"), Kr = /* @__PURE__ */ q("<svg viewBox=\"0 0 44 32\"><use href=#pos-coins-overpaid>"), qr = /* @__PURE__ */ q("<svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=1.8 stroke-linecap=round stroke-linejoin=round><path d=\"M6 3h12M6 21h12M7.5 3v3.5c0 2.6 4.5 4 4.5 5.5s-4.5 2.9-4.5 5.5V21M16.5 3v3.5c0 2.6-4.5 4-4.5 5.5s4.5 2.9 4.5 5.5V21\"></path><path d=\"M8.5 20.2c.8-1.8 2.2-2.6 3.5-2.6s2.7.8 3.5 2.6Z\"fill=currentColor stroke=none>"), Jr = /* @__PURE__ */ q("<svg viewBox=\"0 0 24 24\"fill=currentColor><rect x=10.4 y=3.5 width=3.2 height=11 rx=1.2></rect><circle cx=12 cy=19 r=1.9>"), Yr = /* @__PURE__ */ q("<svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=1.8 stroke-linecap=round><path d=\"M2.5 9a14 14 0 0 1 19 0M5.5 12.5a9.5 9.5 0 0 1 13 0M8.8 15.8a5 5 0 0 1 6.4 0\"></path><circle cx=12 cy=19.5 r=1.1 fill=currentColor stroke=none></circle><path d=\"M4 4l16 16\">"), Xr = /* @__PURE__ */ q("<svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=2.2 stroke-linecap=round><path d=\"M7 7l10 10M17 7 7 17\">"), Zr = /* @__PURE__ */ q("<span aria-hidden=true><!><!><!><!><!><!><!><!><!><!>"), Qr = /* @__PURE__ */ q("<span><!><!>");
function Z(e, t = !1) {
	return t ? "offline" : e.cancelled_at ? "cancelled" : e.error?.includes("Double-spend") ? "double-spend" : e.status === "confirming" && e.confirmations === 0 ? "unconfirmed" : e.status;
}
var $r = {
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
};
function ei(e) {
	return e.confirmations_required <= 0 ? 100 : Math.min(100, Math.max(20, Math.ceil(10 * e.confirmations / e.confirmations_required) * 10));
}
function ti() {
	return Vr();
}
function ni(e) {
	let t = () => Z(e.order, e.offline);
	var n = Zr(), r = n.firstChild, i = r.nextSibling, a = i.nextSibling, o = a.nextSibling, s = o.nextSibling, c = s.nextSibling, l = c.nextSibling, u = l.nextSibling, d = u.nextSibling, f = d.nextSibling;
	return X(n, U(W, {
		get when() {
			return t() === "pending";
		},
		get children() {
			return Hr();
		}
	}), r), X(n, U(W, {
		get when() {
			return t() === "unconfirmed";
		},
		get children() {
			return Ur();
		}
	}), i), X(n, U(W, {
		get when() {
			return t() === "confirming";
		},
		get children() {
			var t = Ur();
			return K(() => `${ei(e.order)}%`, (e) => {
				Tr(t, "--progress", e);
			}), t;
		}
	}), a), X(n, U(W, {
		get when() {
			return t() === "partial";
		},
		get children() {
			return Wr();
		}
	}), o), X(n, U(W, {
		get when() {
			return t() === "paid";
		},
		get children() {
			return Gr();
		}
	}), s), X(n, U(W, {
		get when() {
			return t() === "overpaid";
		},
		get children() {
			return Kr();
		}
	}), c), X(n, U(W, {
		get when() {
			return t() === "expired";
		},
		get children() {
			return qr();
		}
	}), l), X(n, U(W, {
		get when() {
			return t() === "double-spend";
		},
		get children() {
			return Jr();
		}
	}), u), X(n, U(W, {
		get when() {
			return t() === "offline";
		},
		get children() {
			return Yr();
		}
	}), d), X(n, U(W, {
		get when() {
			return t() === "cancelled";
		},
		get children() {
			return Xr();
		}
	}), f), K(() => `pos-icon pos-icon-${t()}`, (e, t) => {
		Y(n, e, t);
	}), n;
}
function ri(e) {
	let t = () => Z(e.order, e.offline);
	var n = Qr(), r = n.firstChild, i = r.nextSibling;
	return X(n, U(ni, {
		get order() {
			return e.order;
		},
		get offline() {
			return e.offline;
		}
	}), r), X(n, () => $r[t()] || e.order.status, i), K(() => `pos-badge state-${t()}`, (e, t) => {
		Y(n, e, t);
	}), n;
}
//#endregion
//#region src/refund.ts
function ii(e) {
	return /^(?:[1-9A-HJ-NP-Za-km-z]{95}|[1-9A-HJ-NP-Za-km-z]{106})$/.test(e);
}
function ai(e) {
	let t = e.trim(), n = /^monero:([^?]+)/i.exec(t);
	return n && (t = decodeURIComponent(n[1])), ii(t) ? t : null;
}
var oi = null;
function si() {
	return window.jsQR ? Promise.resolve(window.jsQR) : (oi ??= new Promise((e, t) => {
		let n = document.createElement("script");
		n.src = "/static/jsQR.js", n.onload = () => window.jsQR ? e(window.jsQR) : t(/* @__PURE__ */ Error("QR decoder unavailable")), n.onerror = () => {
			oi = null, t(/* @__PURE__ */ Error("QR decoder unavailable"));
		}, document.head.appendChild(n);
	}), oi);
}
function ci(e, t, n, r) {
	let i = Math.min(1, 1600 / Math.max(n, r)), a = document.createElement("canvas");
	a.width = Math.max(1, Math.round(n * i)), a.height = Math.max(1, Math.round(r * i));
	let o = a.getContext("2d", { willReadFrequently: !0 });
	return o ? (o.drawImage(t, 0, 0, a.width, a.height), e(o.getImageData(0, 0, a.width, a.height).data, a.width, a.height, { inversionAttempts: "attemptBoth" })?.data ?? null) : null;
}
async function li(e) {
	let t = await si(), n = await createImageBitmap(e);
	try {
		return ci(t, n, n.width, n.height);
	} finally {
		n.close();
	}
}
function ui(e) {
	let t = null, n, r = () => {}, i = () => {
		window.clearTimeout(n), t?.getTracks().forEach((e) => e.stop()), t = null, e.srcObject = null, r(null);
	};
	return {
		result: new Promise((a, o) => {
			r = (e) => {
				r = () => {}, a(e);
			}, (async () => {
				let a = await si();
				t = await navigator.mediaDevices.getUserMedia({
					video: { facingMode: "environment" },
					audio: !1
				}), e.srcObject = t, await e.play();
				let o = () => {
					if (t) {
						if (e.readyState >= 2 && e.videoWidth > 0) {
							let t = ci(a, e, Math.min(e.videoWidth, 640), Math.round(e.videoHeight * Math.min(e.videoWidth, 640) / e.videoWidth));
							if (t) {
								let e = r;
								r = () => {}, i(), e(t);
								return;
							}
						}
						n = window.setTimeout(o, 200);
					}
				};
				o();
			})().catch((e) => {
				r = () => {}, i(), o(e);
			});
		}),
		stop: i
	};
}
//#endregion
//#region src/theme.ts
async function di(e) {
	e === "system" ? delete document.documentElement.dataset.theme : document.documentElement.dataset.theme = e;
	let t = new URLSearchParams({
		theme: e,
		next: location.pathname
	});
	try {
		let e = await fetch("/dashboard/theme", {
			method: "POST",
			body: t,
			redirect: "manual"
		});
		return e.type === "opaqueredirect" || e.ok;
	} catch {
		return !1;
	}
}
//#endregion
//#region src/main.tsx
var fi = /* @__PURE__ */ q("<p class=pos-expiry>Send payment within "), pi = /* @__PURE__ */ q("<p>"), mi = /* @__PURE__ */ q("<p class=pos-pay-caption>"), hi = /* @__PURE__ */ q("<p class=pos-pay-xmr> <span>XMR"), gi = /* @__PURE__ */ q("<p class=pos-pay-fiat>≈ <!> <!>"), _i = /* @__PURE__ */ q("<div class=pos-qr>"), vi = /* @__PURE__ */ q("<p class=pos-quiet-label>Payment address"), yi = /* @__PURE__ */ q("<div class=pos-address><code></code><button type=button aria-label=\"Copy payment address\">"), bi = /* @__PURE__ */ q("<svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=3 stroke-linecap=round stroke-linejoin=round><path d=\"m5 12 5 5L19 7\">"), xi = /* @__PURE__ */ q("<span class=\"pos-spinner pos-spinner-small\">"), Si = /* @__PURE__ */ q("<button type=button><svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=2 aria-hidden=true><path d=\"M3 8V3h5M16 3h5v5M21 16v5h-5M8 21H3v-5M7 7h3v3H7zM14 7h3v3h-3zM7 14h3v3H7zM14 14h3v3h-3z\">"), Ci = /* @__PURE__ */ q("<p class=pos-refund-message role=alert>"), wi = /* @__PURE__ */ q("<section class=pos-pay-card aria-label=\"Payment details\"><!><!><hr><label class=pos-field-label for=pos-refund>Refund address <span>(optional)</span></label><div><input id=pos-refund type=text autocomplete=off placeholder=\"Your Monero refund address\"aria-describedby=pos-refund-note><span class=pos-refund-state role=status><!><!></span></div><div class=pos-refund-tools><button type=button><svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=2 stroke-linejoin=round aria-hidden=true><rect x=3 y=4 width=18 height=16 rx=1.5></rect><circle cx=9 cy=10 r=1.6></circle><path d=\"m3 17 5-5 4 4 3-3 6 6\"></path></svg>Choose QR image</button><input type=file accept=image/* hidden></div><video class=pos-camera autoplay playsinline muted></video><p class=pos-note id=pos-refund-note>Recorded for the merchant if a refund is needed. Refunds are not sent automatically."), Ti = /* @__PURE__ */ q("<p class=pos-pay-caption>Received"), Ei = /* @__PURE__ */ q("<p class=pos-pay-fiat>for <!> <!>"), Di = /* @__PURE__ */ q("<section><p class=pos-outcome-title></p><p></p><p class=pos-outcome-amount> XMR<!>"), Oi = /* @__PURE__ */ q("<button class=pos-back type=button aria-label=\"Back to POS\"><svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=2.2 stroke-linecap=round stroke-linejoin=round aria-hidden=true><path d=\"m15 5-7 7 7 7\">"), ki = /* @__PURE__ */ q("<strong>POS"), Ai = /* @__PURE__ */ q("<button class=pos-orders-link type=button aria-label=\"All orders\"title=\"All orders\"><svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=2 stroke-linecap=round aria-hidden=true><path d=\"M9 6h11M9 12h11M9 18h11\"></path><circle cx=4.5 cy=6 r=1 fill=currentColor></circle><circle cx=4.5 cy=12 r=1 fill=currentColor></circle><circle cx=4.5 cy=18 r=1 fill=currentColor>"), ji = /* @__PURE__ */ q("<header class=pos-top><span class=pos-top-end><span class=pos-site-controls>"), Mi = /* @__PURE__ */ q("<section class=pos-stack aria-label=\"Background orders\"><div class=pos-stack-heading><strong>Background orders · </strong><button type=button>View all →</button></div><div class=pos-stack-scroll tabindex=0 aria-label=\"Background orders, scroll sideways\">"), Ni = /* @__PURE__ */ q("<p class=pos-error role=alert>"), Pi = /* @__PURE__ */ q("<main><p aria-live=polite><span></span></p><div class=pos-keys></div><div class=pos-field><label class=pos-field-label for=pos-reference>Reference <span>(optional)</span></label><input id=pos-reference class=pos-input type=text maxlength=120 placeholder=\"E.g. customer name or note\"autocomplete=off></div><button type=button class=pos-primary>"), Fi = /* @__PURE__ */ q("<p class=pos-list-note>Completed on this device since the POS was opened. They clear after 24 hours or when the page reloads. <a>See all orders →"), Ii = /* @__PURE__ */ q("<p class=pos-error role=alert> <button type=button class=pos-link>Retry"), Li = /* @__PURE__ */ q("<p class=pos-empty>No matches from this session. <a>Search all orders →"), Ri = /* @__PURE__ */ q("<main class=pos-list><h1>Orders</h1><p class=pos-list-subtitle>Choose an order to open.</p><div class=pos-search><svg viewBox=\"0 0 24 24\"fill=none stroke=currentColor stroke-width=2 stroke-linecap=round aria-hidden=true><circle cx=10.5 cy=10.5 r=6></circle><path d=\"m15 15 5 5\"></path></svg><input class=pos-input type=search aria-label=\"Search reference or order ID\"placeholder=\"Search reference or order ID\"></div><div class=pos-tabs role=tablist aria-label=\"Order status\"><button type=button role=tab>Active · </button><button type=button role=tab>Finished · </button></div><!><!><div class=pos-list-items>"), zi = /* @__PURE__ */ q("<a class=pos-store>"), Bi = /* @__PURE__ */ q("<button type=button><span class=pos-stack-ref></span><span class=pos-stack-amount>"), Vi = /* @__PURE__ */ q("<button type=button>"), Hi = /* @__PURE__ */ q("<svg viewBox=\"0 0 28 20\"fill=none stroke=currentColor stroke-width=2.4 stroke-linejoin=round aria-hidden=true><path d=\"M9 2h16a1.5 1.5 0 0 1 1.5 1.5v13A1.5 1.5 0 0 1 25 18H9l-7.5-8Z\"></path><path d=\"m13 6.5 8 7m0-7-8 7\"stroke-linecap=round>"), Ui = /* @__PURE__ */ q("<button type=button class=pos-primary>Background order"), Wi = /* @__PURE__ */ q("<button type=button class=pos-cancel>Cancel order"), Gi = /* @__PURE__ */ q("<p class=pos-action-hint>Background keeps this payment open · Cancel asks for confirmation"), Ki = /* @__PURE__ */ q("<main class=pos-payment><div class=pos-order-heading><div><h1></h1><p>Order <!></p></div></div><!><!><!>"), qi = /* @__PURE__ */ q("<main class=pos-payment><p class=pos-loading>Loading order…"), Ji = /* @__PURE__ */ q("<button type=button class=pos-primary>New order"), Yi = /* @__PURE__ */ q("<p class=pos-action-hint>Background keeps this payment open while you serve the next customer"), Xi = /* @__PURE__ */ q("<p class=pos-empty>"), Zi = /* @__PURE__ */ q("<article class=pos-order-card><div class=pos-order-card-head><div><h2></h2><p></p></div></div><p class=pos-order-sum> <span></span></p><div class=pos-order-foot><small></small><button type=button class=pos-link>Open →"), Qi = document.getElementById("pos-root");
if (!Qi) throw Error("POS root missing");
var Q = {
	connectionId: Qi.dataset.connectionId || "",
	publicKey: Qi.dataset.publicKey || "",
	currency: Qi.dataset.currency || "AUD",
	decimals: Number(Qi.dataset.decimals || "2"),
	storeName: Qi.dataset.storeName || "Store"
}, $i = `/dashboard/stores/${encodeURIComponent(Q.connectionId)}/pos`, $ = (e) => !!e.cancelled_at || [
	"paid",
	"overpaid",
	"expired"
].includes(e.status);
function ea(e) {
	let t = e.replace(/^order_/, "");
	return t.length <= 10 ? `#${t}` : `#${t.slice(0, 4)}…${t.slice(-4)}`;
}
var ta = (e) => e.merchant_order_id || ea(e.order_id), na = (e) => e.includes(".") ? e.replace(/0+$/, "").replace(/\.$/, "") : e, ra = (e) => {
	let t = e.padStart(Q.decimals + 1, "0");
	return Q.decimals ? `${t.slice(0, -Q.decimals) || "0"}.${t.slice(-Q.decimals)}` : t;
}, ia = (e) => {
	let [t, n] = ra(e).split("."), r = t.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
	return n === void 0 ? r : `${r}.${n}`;
};
function aa(e, t) {
	let n = Math.max(0, e - t), r = Math.floor(n / 86400), i = Math.floor(n % 86400 / 3600), a = Math.floor(n % 3600 / 60);
	return r ? i ? `${r}d ${i}h` : `${r}d` : i ? a ? `${i}h ${a}m` : `${i}h` : a ? `${a}m` : "less than a minute";
}
var oa = (e) => (/* @__PURE__ */ new Date(e * 1e3)).toLocaleTimeString([], {
	hour: "2-digit",
	minute: "2-digit",
	hourCycle: "h23"
});
function sa(e) {
	if (e.length < 40) return e;
	let t = Math.floor(e.length / 2);
	return `${e.slice(0, 5)}…${e.slice(t - 9, t + 9)}…${e.slice(-4)}`;
}
async function ca(e, t) {
	let n = await fetch(e, t);
	if (!n.ok) {
		let e = await n.json().catch(() => ({}));
		throw Error(e.error || `Request failed (${n.status})`);
	}
	return n.status === 204 ? void 0 : n.json();
}
var la = (e, t) => ca(e, {
	method: "POST",
	headers: t === void 0 ? void 0 : { "content-type": "application/json" },
	body: t === void 0 ? void 0 : JSON.stringify(t)
}), [ua, da] = H(Math.floor(Date.now() / 1e3)), fa = 86400;
window.setInterval(() => da(Math.floor(Date.now() / 1e3)), 15e3);
function pa(e) {
	let t = document.getElementById("pos-site-controls");
	if (!t) return;
	let n = t.querySelector(".theme-toggle");
	t.querySelector(".nav-theme-form")?.addEventListener("submit", (e) => {
		let t = e.submitter?.value;
		t && n && (e.preventDefault(), n.className = `theme-toggle theme-toggle-${t}`, n.querySelectorAll("button[name=\"theme\"]").forEach((e) => e.setAttribute("aria-pressed", String(e.value === t))), di(t));
	}), e.append(...Array.from(t.children)), t.remove();
}
function ma(e) {
	let t = Qn(() => e.order.qr_svg), [n, r] = H(!1), [i, a] = H(e.order.refund_address || ""), [o, s] = H(e.order.refund_address || ""), [c, l] = H(e.order.refund_address ? "saved" : "idle"), [u, d] = H(""), [f, p] = H(!1), m, h, g = null, _;
	wn(() => {
		g?.(), window.clearTimeout(_);
	});
	let v = () => ["pending", "partial"].includes(e.order.status) && !e.order.cancelled_at, y = () => e.order.status === "partial", b = () => {
		let t = e.order;
		switch (Z(t)) {
			case "double-spend": return t.error || "Double spend detected. Do not treat this payment as paid.";
			case "unconfirmed": return "Payment seen. Waiting for its first confirmation.";
			case "confirming": return `Payment seen · ${t.confirmations} of ${t.confirmations_required} confirmations`;
			case "partial": return `${na(t.received_xmr || "0")} of ${na(t.xmr_amount)} XMR received`;
			default: return "";
		}
	};
	async function ee() {
		try {
			await navigator.clipboard.writeText(e.order.address), r(!0), window.setTimeout(() => r(!1), 2e3);
		} catch {}
	}
	async function te(t) {
		if (ii(t) && t !== o()) {
			l("saving"), d("");
			try {
				let n = await fetch(`/pay/${encodeURIComponent(Q.publicKey)}/orders/${encodeURIComponent(e.order.order_id)}/refund-address`, {
					method: "POST",
					headers: {
						accept: "application/json",
						"content-type": "application/x-www-form-urlencoded"
					},
					body: new URLSearchParams({ refund_address: t })
				}), r = await n.json().catch(() => ({}));
				if (i().trim() !== t) return;
				n.ok ? (s(t), l("saved")) : (l("invalid"), d(r.error || "That address was not accepted."));
			} catch {
				i().trim() === t && (l("idle"), d("Could not save. Check the connection and try again."));
			}
		}
	}
	function ne(e) {
		a(e), d("");
		let t = e.trim();
		if (window.clearTimeout(_), !t) {
			l("idle");
			return;
		}
		if (t === o()) {
			l("saved");
			return;
		}
		if (!ii(t)) {
			l(t.length >= 95 ? "invalid" : "idle");
			return;
		}
		l("idle"), _ = window.setTimeout(() => void te(t), 500);
	}
	function re(e) {
		let t = e ? ai(e) : null;
		if (!t) {
			d(e ? "That QR code does not contain a Monero address." : "No QR code found in that image.");
			return;
		}
		a(t), te(t);
	}
	async function x() {
		if (f()) {
			g?.();
			return;
		}
		if (d(""), !m) return;
		let e = ui(m);
		g = e.stop, p(!0);
		try {
			let t = await e.result;
			t && re(t);
		} catch {
			d("Camera unavailable. Choose a QR image instead.");
		} finally {
			p(!1), g = null;
		}
	}
	async function S(e) {
		if (e) {
			d("");
			try {
				re(await li(e));
			} catch {
				d("Could not read that image. Choose another file.");
			}
			h && (h.value = "");
		}
	}
	var C = wi(), w = C.firstChild, T = w.nextSibling, E = T.nextSibling, D = E.nextSibling.nextSibling, O = D.firstChild, k = O.nextSibling, A = k.firstChild, ie = A.nextSibling, j = D.nextSibling, ae = j.firstChild, oe = ae.nextSibling, se = j.nextSibling, ce = se.nextSibling;
	return X(C, U(W, {
		get when() {
			return v();
		},
		get children() {
			var t = fi();
			return t.firstChild, X(t, () => aa(e.order.expires_at, ua()), null), t;
		}
	}), w), X(C, U(W, {
		get when() {
			return b();
		},
		get children() {
			var t = pi();
			return X(t, b), K(() => `pos-pay-detail state-${Z(e.order)}`, (e, n) => {
				Y(t, e, n);
			}), t;
		}
	}), T), X(C, U(W, {
		get when() {
			return v();
		},
		get fallback() {
			return [
				Ti(),
				(() => {
					var t = hi(), n = t.firstChild;
					return X(t, () => na(e.order.received_xmr || e.order.xmr_amount), n), t;
				})(),
				U(W, {
					get when() {
						return e.order.currency !== "XMR";
					},
					get children() {
						var t = Ei(), n = t.firstChild.nextSibling, r = n.nextSibling.nextSibling;
						return X(t, () => e.order.amount, n), X(t, () => e.order.currency, r), t;
					}
				})
			];
		},
		get children() {
			return [
				(() => {
					var e = mi();
					return X(e, () => y() ? "Send the remaining amount" : "Send exactly this amount"), e;
				})(),
				(() => {
					var t = hi(), n = t.firstChild;
					return X(t, () => na(y() && e.order.remaining_xmr || e.order.xmr_amount), n), t;
				})(),
				U(W, {
					get when() {
						return or(() => e.order.currency !== "XMR")() && !y();
					},
					get children() {
						var t = gi(), n = t.firstChild.nextSibling, r = n.nextSibling.nextSibling;
						return X(t, () => e.order.amount, n), X(t, () => e.order.currency, r), t;
					}
				}),
				U(W, {
					get when() {
						return t();
					},
					get children() {
						var e = _i();
						return K(() => t(), (t) => {
							e.innerHTML = t;
						}), e;
					}
				}),
				vi(),
				(() => {
					var t = yi(), r = t.firstChild, i = r.nextSibling;
					return X(r, () => sa(e.order.address)), i._$$click = () => void ee(), X(i, () => n() ? "Copied" : "Copy"), K(() => e.order.address, (e) => {
						J(r, "title", e);
					}), t;
				})()
			];
		}
	}), E), O.addEventListener("blur", () => void te(i().trim())), O._$$input = (e) => ne(e.currentTarget.value), X(k, U(W, {
		get when() {
			return c() === "saved";
		},
		get children() {
			return bi();
		}
	}), A), X(k, U(W, {
		get when() {
			return c() === "saving";
		},
		get children() {
			return xi();
		}
	}), ie), X(j, U(W, {
		get when() {
			return navigator.mediaDevices?.getUserMedia;
		},
		get children() {
			var e = Si();
			return e.firstChild, e._$$click = () => void x(), X(e, () => f() ? "Stop camera" : "Scan refund QR", null), e;
		}
	}), ae), ae._$$click = () => h?.click(), oe.addEventListener("change", (e) => void S(e.currentTarget.files?.[0])), Dr(() => (e) => {
		h = e;
	}, oe), Dr(() => (e) => {
		m = e;
	}, se), X(C, U(W, {
		get when() {
			return u();
		},
		get children() {
			var e = Ci();
			return X(e, u), e;
		}
	}), ce), K(() => ({
		e: `pos-refund state-${c()}`,
		t: i(),
		a: c() === "invalid" ? "true" : "false",
		o: c() === "saving" ? "Saving refund address" : c() === "saved" ? "Refund address saved" : "",
		i: !f()
	}), ({ e, t, a: n, o: r, i }, a) => {
		Y(D, e, a?.e), O.value = t ?? "", n !== a?.a && J(O, "aria-invalid", n), r !== a?.o && J(k, "aria-label", r), i !== a?.i && J(se, "hidden", i);
	}), C;
}
function ha(e) {
	let t = () => {
		let t = e.order;
		return t.cancelled_at ? t.status === "pending" ? "This order was cancelled. If money still arrives at its address, it will show in the order for review." : "Payment activity arrived after this order was cancelled. Review it in the order details." : t.error ? t.error : t.status === "paid" ? "Payment received and confirmed." : t.status === "expired" ? "This order expired before it was paid." : $r[Z(t)] || t.status;
	};
	var n = Di(), r = n.firstChild, i = r.nextSibling, a = i.nextSibling, o = a.firstChild, s = o.nextSibling;
	return X(n, U(ni, { get order() {
		return e.order;
	} }), r), X(r, () => $r[Z(e.order)]), X(i, t), X(a, () => na(e.order.xmr_amount), o), X(a, U(W, {
		get when() {
			return e.order.currency !== "XMR";
		},
		get children() {
			return [
				" · ",
				or(() => e.order.amount),
				" ",
				or(() => e.order.currency)
			];
		}
	}), s), K(() => `pos-outcome state-${Z(e.order)}`, (e, t) => {
		Y(n, e, t);
	}), n;
}
function ga() {
	let [e, t] = H([]), [n, r] = H("keypad"), [i, a] = H(null), [o, s] = H("0"), [c, l] = H(""), [u, d] = H(""), [f, p] = H(!1), [m, h] = H(!1), [g, _] = H(""), [v, y] = H("active"), [b, ee] = H(0), [te, ne] = H({}), re = /* @__PURE__ */ new Set(), x = [], S = Qn(() => e().find((e) => e.order_id === i()) || null), C = Qn(() => e().filter((e) => !$(e) && e.order_id !== i())), w = Qn(() => e().filter((e) => !$(e))), T = Qn(() => {
		let t = te();
		return e().filter((e) => $(e) && t[e.order_id] !== void 0 && ua() - t[e.order_id] < fa).sort((e, n) => t[n.order_id] - t[e.order_id]);
	}), E = Qn(() => {
		let e = g().trim().toLowerCase();
		return (v() === "active" ? w() : T()).filter((t) => !e || t.order_id.toLowerCase().includes(e) || (t.merchant_order_id || "").toLowerCase().includes(e));
	}), D = Qn(() => ia(o())), O = null, k, A, ie = null, j = (e = "") => `/dashboard/stores/${encodeURIComponent(Q.connectionId)}/orders${e ? `?q=${encodeURIComponent(e)}` : ""}`;
	function ae(e, t) {
		return e.updated_at !== void 0 && t.updated_at !== void 0 && t.updated_at < e.updated_at ? e : {
			...e,
			...t,
			qr_svg: t.qr_svg ?? e.qr_svg,
			refund_address: t.refund_address === void 0 ? e.refund_address : t.refund_address
		};
	}
	function oe(e) {
		let n = e(x);
		x = n, t(n);
		for (let e of n) $(e) || re.add(e.order_id);
		let r = te(), i = n.filter((e) => $(e) && re.has(e.order_id) && r[e.order_id] === void 0);
		if (i.length) {
			let e = Math.floor(Date.now() / 1e3);
			ne({
				...r,
				...Object.fromEntries(i.map((t) => [t.order_id, e]))
			});
		}
	}
	function se(e) {
		oe((t) => t.some((t) => t.order_id === e.order_id) ? t.map((t) => t.order_id === e.order_id ? ae(t, e) : t) : [e, ...t]);
	}
	function ce(e) {
		_(e), queueMicrotask(P);
	}
	async function M() {
		try {
			let e = await ca(`${$i}/orders?state=active`), t = new Set(e.orders.map((e) => e.order_id)), n = x.filter((e) => !$(e) && !t.has(e.order_id)).map((e) => e.order_id);
			oe((r) => [...e.orders.map((e) => {
				let t = r.find((t) => t.order_id === e.order_id);
				return t ? ae(t, e) : e;
			}), ...r.filter((e) => !t.has(e.order_id) && ($(e) || n.includes(e.order_id)))]);
			for (let e of n) N(e).catch(() => {});
			if (!i()) {
				let t = e.orders.find((e) => !e.backgrounded);
				t && (a(t.order_id), r("payment"), N(t.order_id).catch(() => {}));
			}
			d(""), queueMicrotask(P);
		} catch (e) {
			d(e.message);
		}
	}
	async function N(e) {
		let t = await ca(`${$i}/orders/${encodeURIComponent(e)}`);
		return se(t), t;
	}
	function le() {
		let t = S() && !$(S()) ? [S().order_id] : [], r = n() === "list" ? E().filter((e) => !$(e)).map((e) => e.order_id) : C().map((e) => e.order_id), i = e().filter((e) => !$(e)).map((e) => e.order_id), a = e().filter((e) => e.cancelled_at && e.status === "pending").slice(0, 8).map((e) => e.order_id);
		return [.../* @__PURE__ */ new Set([
			...t,
			...r,
			...i,
			...a
		])].slice(0, 32);
	}
	function P() {
		O?.close(), O = null;
		let e = le();
		if (!e.length) {
			window.clearTimeout(k), k = void 0, h(!1);
			return;
		}
		let t = new EventSource(`${$i}/events?orders=${e.map(encodeURIComponent).join(",")}`);
		O = t, t.addEventListener("open", () => {
			O === t && (window.clearTimeout(k), k = void 0, h(!1));
		}), t.addEventListener("status", (e) => {
			if (O === t) try {
				let t = JSON.parse(e.data);
				oe((e) => e.map((e) => e.order_id === t.order_id ? ae(e, t) : e)), t.is_terminal && queueMicrotask(P);
			} catch {}
		}), t.addEventListener("error", () => {
			O === t && k === void 0 && (k = window.setTimeout(() => h(!0), 6e3));
		});
	}
	function ue() {
		s("0"), l(""), a(null), r("keypad"), d("");
	}
	function de(e) {
		s((t) => (t + e).slice(-(Q.decimals + 9)).replace(/^0+(?=\d)/, "") || "0");
	}
	function fe() {
		s((e) => e.length > 1 ? e.slice(0, -1) : "0");
	}
	async function pe() {
		if (f() || /^0+$/.test(o())) return;
		p(!0), d("");
		let e = ra(o()), t = c().trim();
		(!ie || ie.amount !== e || ie.reference !== t) && (ie = {
			amount: e,
			reference: t,
			key: crypto.randomUUID()
		});
		try {
			let n = await ca(`${$i}/orders`, {
				method: "POST",
				headers: { "content-type": "application/json" },
				body: JSON.stringify({
					amount: e,
					merchant_order_id: t || null,
					request_key: ie.key
				})
			});
			await N(n.order_id), ie = null, a(n.order_id), r("payment"), queueMicrotask(P);
		} catch (e) {
			d(e.message);
		} finally {
			p(!1);
		}
	}
	async function me() {
		let e = S();
		if (e && !f()) {
			p(!0), d("");
			try {
				await la(`${$i}/orders/${encodeURIComponent(e.order_id)}/background`), se({
					...e,
					backgrounded: !0
				}), ue(), queueMicrotask(P);
			} catch (e) {
				d(e.message);
			} finally {
				p(!1);
			}
		}
	}
	async function F() {
		let e = S();
		if (e && !f() && window.confirm(`Cancel ${ta(e)}? The payment address has already been issued; any later payment will still need review.`)) {
			p(!0), d("");
			try {
				await la(`${$i}/orders/${encodeURIComponent(e.order_id)}/cancel`), await N(e.order_id), queueMicrotask(P);
			} catch (t) {
				d(t.message), N(e.order_id).catch(() => {});
			} finally {
				p(!1);
			}
		}
	}
	async function he(e) {
		n() === "list" && A && ee(A.scrollTop), d(""), a(e.order_id), r("payment");
		try {
			await N(e.order_id), queueMicrotask(P);
		} catch (e) {
			d(e.message);
		}
	}
	function ge() {
		r("list"), queueMicrotask(() => {
			A && (A.scrollTop = b()), P();
		});
	}
	function _e(e) {
		if (n() === "keypad") {
			if (e.target instanceof HTMLInputElement) {
				e.key === "Enter" && pe();
				return;
			}
			/^[0-9]$/.test(e.key) ? de(e.key) : e.key === "Backspace" ? fe() : e.key === "Escape" ? s("0") : e.key === "Enter" && pe();
		}
	}
	document.addEventListener("keydown", _e), queueMicrotask(() => {
		M();
	}), wn(() => {
		document.removeEventListener("keydown", _e), O?.close(), window.clearTimeout(k);
	});
	let ve = (e) => `${e.merchant_order_id ? "Reference · " : ""}${ea(e.order_id)} · created ${oa(e.created_at)}`, ye = (e) => {
		if (e.cancelled_at) return e.status === "pending" ? "Cancelled before payment" : "Payment after cancellation · review";
		switch (Z(e)) {
			case "pending": return `Expires in ${aa(e.expires_at, ua())}`;
			case "unconfirmed": return "Payment seen, not yet confirmed";
			case "confirming": return `${e.confirmations} of ${e.confirmations_required} confirmations`;
			case "partial": return "Waiting for remaining amount";
			case "double-spend": return "Double spend detected · do not treat as paid";
			case "paid": return "Settled";
			case "overpaid": return "Extra amount received · review";
			case "expired": return "Expired unpaid";
			default: return $r[Z(e)] || e.status;
		}
	};
	return [
		U(ti, {}),
		(() => {
			var e = ji(), t = e.firstChild, i = t.firstChild;
			X(e, U(W, {
				get when() {
					return n() === "list";
				},
				get fallback() {
					var e = zi();
					return Cr(e), X(e, () => Q.storeName), K(() => `/dashboard/stores/${encodeURIComponent(Q.connectionId)}`, (t) => {
						J(e, "href", t);
					}), e;
				},
				get children() {
					return [(() => {
						var e = Oi();
						return e._$$click = () => {
							r("keypad"), queueMicrotask(P);
						}, e;
					})(), ki()];
				}
			}), t), X(t, U(W, {
				get when() {
					return n() !== "list";
				},
				get children() {
					var e = Ai();
					return e._$$click = ge, e;
				}
			}), i);
			var a = pa;
			return typeof a == "function" || Array.isArray(a) ? Dr(() => a, i) : pa = i, e;
		})(),
		U(W, {
			get when() {
				return or(() => n() === "keypad")() && C().length > 0;
			},
			get children() {
				var e = Mi(), t = e.firstChild, n = t.firstChild;
				n.firstChild;
				var r = n.nextSibling, i = t.nextSibling;
				return X(n, () => C().length, null), r._$$click = ge, i.addEventListener("wheel", (e) => {
					let t = e.currentTarget;
					t.scrollWidth > t.clientWidth && Math.abs(e.deltaY) > Math.abs(e.deltaX) && (t.scrollLeft += e.deltaY, e.preventDefault());
				}), X(i, U(nr, {
					get each() {
						return C();
					},
					children: (e) => (() => {
						var t = Bi(), n = t.firstChild, r = n.nextSibling;
						return t._$$click = () => void he(e), X(t, U(ni, {
							order: e,
							get offline() {
								return m();
							}
						}), n), X(n, () => ta(e)), X(r, () => e.amount), K(() => ({
							e: `pos-stack-card state-${Z(e, m())}`,
							t: `${ta(e)} · ${$r[Z(e, m())]} · ${e.amount} ${e.currency}`,
							a: `Open ${ta(e)}, ${$r[Z(e, m())]}, ${e.amount} ${e.currency}`
						}), ({ e, t: n, a: r }, i) => {
							Y(t, e, i?.e), n !== i?.t && J(t, "title", n), r !== i?.a && J(t, "aria-label", r);
						}), t;
					})()
				})), e;
			}
		}),
		U(W, {
			get when() {
				return n() === "keypad";
			},
			get children() {
				var e = Pi(), t = e.firstChild, n = t.firstChild, r = t.nextSibling, i = r.nextSibling, a = i.firstChild.nextSibling, d = i.nextSibling;
				return X(t, D, n), X(n, () => Q.currency), X(r, U(nr, {
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
						var t = Vi();
						return t._$$click = () => e === "C" ? s("0") : e === "⌫" ? fe() : de(e), J(t, "aria-label", e === "C" ? "Clear" : e === "⌫" ? "Backspace" : e), X(t, e === "⌫" ? Hi() : e), K(() => wr(e === "C" ? "clear" : e === "⌫" ? "delete" : ""), (e, n) => {
							Y(t, e, n);
						}), t;
					})()
				})), a._$$input = (e) => l(e.currentTarget.value), X(e, U(W, {
					get when() {
						return u();
					},
					get children() {
						var e = Ni();
						return X(e, u), e;
					}
				}), d), d._$$click = () => void pe(), X(d, () => f() ? "Creating order…" : "Charge"), K(() => ({
					e: `pos-keypad ${C().length ? "has-stack" : ""}`,
					t: `pos-amount len-${Math.min(4, Math.floor(D().length / 6))}`,
					a: c(),
					o: /^0+$/.test(o()) || f()
				}), ({ e: n, t: r, a: i, o }, s) => {
					Y(e, n, s?.e), Y(t, r, s?.t), a.value = i ?? "", o !== s?.o && J(d, "disabled", o);
				}), e;
			}
		}),
		U(W, {
			get when() {
				return or(() => n() === "payment")() ? i() : null;
			},
			keyed: !0,
			children: (e) => U(W, {
				get when() {
					return S();
				},
				get fallback() {
					return qi();
				},
				get children() {
					var e = Ki(), t = e.firstChild, n = t.firstChild.firstChild, r = n.nextSibling, i = r.firstChild, a = i.nextSibling, o = t.nextSibling, s = o.nextSibling, c = s.nextSibling;
					return X(n, () => ta(S())), X(r, () => S().merchant_order_id ? "Reference · " : "", i), X(r, () => ea(S().order_id), a), X(t, U(ri, {
						get order() {
							return S();
						},
						get offline() {
							return or(() => !!m())() ? !$(S()) : m();
						}
					}), null), X(e, U(W, {
						get when() {
							return !$(S());
						},
						get fallback() {
							return U(ha, { get order() {
								return S();
							} });
						},
						get children() {
							return U(ma, { get order() {
								return S();
							} });
						}
					}), o), X(e, U(W, {
						get when() {
							return u();
						},
						get children() {
							var e = Ni();
							return X(e, u), e;
						}
					}), s), X(e, U(W, {
						get when() {
							return !$(S());
						},
						get fallback() {
							var e = Ji();
							return e._$$click = ue, e;
						},
						get children() {
							return [(() => {
								var e = Ui();
								return e._$$click = () => void me(), K(() => f(), (t) => {
									J(e, "disabled", t);
								}), e;
							})(), U(W, {
								get when() {
									return or(() => S().status === "pending")() && !S().error;
								},
								get fallback() {
									return Yi();
								},
								get children() {
									return [(() => {
										var e = Wi();
										return e._$$click = () => void F(), K(() => f(), (t) => {
											J(e, "disabled", t);
										}), e;
									})(), Gi()];
								}
							})];
						}
					}), c), e;
				}
			})
		}),
		U(W, {
			get when() {
				return n() === "list";
			},
			get children() {
				var e = Ri(), t = e.firstChild.nextSibling.nextSibling, n = t.firstChild.nextSibling, r = t.nextSibling, i = r.firstChild;
				i.firstChild;
				var a = i.nextSibling;
				a.firstChild;
				var o = r.nextSibling, s = o.nextSibling, c = s.nextSibling;
				return Dr(() => (e) => {
					A = e;
				}, e), n._$$input = (e) => ce(e.currentTarget.value), i._$$click = () => {
					y("active"), queueMicrotask(P);
				}, X(i, () => w().length, null), a._$$click = () => {
					y("finished"), queueMicrotask(P);
				}, X(a, () => T().length, null), X(e, U(W, {
					get when() {
						return v() === "finished";
					},
					get children() {
						var e = Fi(), t = e.firstChild.nextSibling;
						return Cr(t), K(() => j(), (e) => {
							J(t, "href", e);
						}), e;
					}
				}), o), X(e, U(W, {
					get when() {
						return u();
					},
					get children() {
						var e = Ii(), t = e.firstChild, n = t.nextSibling;
						return X(e, u, t), n._$$click = () => void M(), e;
					}
				}), s), X(e, U(W, {
					get when() {
						return or(() => !u())() && E().length === 0;
					},
					get children() {
						return U(W, {
							get when() {
								return g().trim();
							},
							get fallback() {
								var e = Xi();
								return X(e, () => `No ${v()} orders yet.`), e;
							},
							get children() {
								var e = Li(), t = e.firstChild.nextSibling;
								return Cr(t), K(() => j(g().trim()), (e) => {
									J(t, "href", e);
								}), e;
							}
						});
					}
				}), c), X(c, U(nr, {
					get each() {
						return E();
					},
					children: (e) => (() => {
						var t = Zi(), n = t.firstChild, r = n.firstChild.firstChild, i = r.nextSibling, a = n.nextSibling, o = a.firstChild, s = o.nextSibling, c = a.nextSibling.firstChild, l = c.nextSibling;
						return X(r, () => ta(e)), X(i, () => ve(e)), X(n, U(ri, {
							order: e,
							get offline() {
								return or(() => !!m())() ? !$(e) : m();
							}
						}), null), X(a, () => e.amount, o), X(s, () => e.currency), X(c, () => ye(e)), l._$$click = () => void he(e), t;
					})()
				})), K(() => ({
					e: g(),
					t: v() === "active" ? "true" : "false",
					a: wr(v() === "active" ? "selected" : ""),
					o: v() === "finished" ? "true" : "false",
					i: wr(v() === "finished" ? "selected" : "")
				}), ({ e, t, a: r, o, i: s }, c) => {
					n.value = e ?? "", t !== c?.t && J(i, "aria-selected", t), Y(i, r, c?.a), o !== c?.o && J(a, "aria-selected", o), Y(a, s, c?.i);
				}), e;
			}
		})
	];
}
pr(() => U(ga, {}), Qi), hr(["click", "input"]);
//#endregion

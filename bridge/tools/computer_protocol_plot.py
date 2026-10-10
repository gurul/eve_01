"""Recompute comparison figures from raw evaluation rows; no invented performance values."""
from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import tempfile
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import fastlane_eval as fe  # noqa: E402
import matplotlib.pyplot as plt  # noqa: E402
from computer_protocol_eval import ADMISSION_ARMS, ADMISSION_LIMITATION, ARMS, metrics  # noqa: E402
from PIL import Image, ImageChops  # noqa: E402


def percentile(values, q):
    values = sorted(values)
    return values[min(len(values)-1, int(len(values)*q))] if values else None


def summaries(folder):
    ax = json.loads((folder/"ax.json").read_text())
    visual = json.loads((folder/"visual.json").read_text())
    files = sorted((folder/"browser").glob("runs-*.jsonl"))
    browser = [json.loads(line) for p in files for line in p.read_text().splitlines() if line.strip()]
    result = {"ax": metrics(ax["rows"]), "visual": {}, "browser": {}, "sources": {}}
    if ax.get("schema") == 2:
        result["admission"] = {arm: result["ax"][arm] for arm in ADMISSION_ARMS}
        result["admission_limitation"] = ADMISSION_LIMITATION
    fixtures = fe.load_sets(Path("bridge/tests/fixtures/ax"))
    router_rows = [fe.route_case(f,c) for group in fixtures.values() for f in group for c in f.cases]
    result["guarded_keyword_admission"] = fe.router_summary(router_rows)
    result["guarded_keyword_admission"]["limitation"] = "Actual router with recording effectors; post-click change mocked. Admission only."
    for provider in ("Decisions", "Jev"):
        subset = [r for r in visual["rows"] if r["provider"] == provider]
        targets = [r for r in subset if r["kind"] == "target"]
        clicks = [r for r in targets if r["expected"] != "none"]
        pressed = [r for r in targets if r["picked"] != "none"]
        verified = [r for r in subset if r["kind"] == "verification"]
        positive = [r for r in verified if r["expected"]]
        negative = [r for r in verified if not r["expected"]]
        result["visual"][provider] = {
            "n":len(subset), "targets":len(targets), "target_clicks":len(clicks),
            "correct_clicks":sum(r["correct"] for r in clicks),
            "wrong_clicks":sum(not r["correct"] for r in pressed),
            "coverage":sum(r["correct"] for r in clicks)/len(clicks),
            "target_p50_ms":statistics.median(r["ms"] for r in targets),
            "verification_p50_ms":statistics.median(r["ms"] for r in verified),
            "verification_n":len(verified), "verification_accuracy":sum(r["correct"] for r in verified)/len(verified),
            "true_positive_rate":sum(r["picked"] for r in positive)/len(positive),
            "false_positive_rate":sum(r["picked"] for r in negative)/len(negative),
            "errors":sum(bool(r["error"]) for r in subset)}
    for model in sorted({r["model"] for r in browser}):
        subset = [r for r in browser if r["model"] == model]
        times = [r["secs"] for r in subset]
        result["browser"][model] = {
            "n":len(subset), "goal_reached":sum(r["success"] for r in subset),
            "autonomous_success":sum(r["success"] and r["outcome"]=="finished" for r in subset),
            "handoffs":sum(r["outcome"] in ("nothing","part_done") for r in subset),
            "errors":sum(r["outcome"]=="error" for r in subset),
            "p50_secs":statistics.median(times), "p90_secs":percentile(times,.9)}
    for p in [folder/"ax.json", folder/"visual.json", *files]:
        result["sources"][str(p.relative_to(folder))] = hashlib.sha256(p.read_bytes()).hexdigest()
    for group in fixtures.values():
        for f in group:
            result["sources"][str(f.path)] = hashlib.sha256(f.path.read_bytes()).hexdigest()
    return result


def normalize_svg(path):
    """Remove serializer trailing whitespace without changing SVG element geometry."""
    path.write_text("\n".join(line.rstrip() for line in path.read_text().splitlines()) + "\n")


def draw(folder, data):
    plt.rcParams.update({"font.size":10,"axes.spines.top":False,"axes.spines.right":False})
    fig, axes = plt.subplots(3,2,figsize=(14,13),layout="constrained")
    fig.suptitle("Eve computer-use evaluation • October 7, 2026", fontsize=20, weight="bold")
    axdata = data["ax"]
    names = list(ARMS)
    short = [s.replace("Keyword","Raw keyword").replace(" (AX only)","\n(AX only)").replace(" → "," →\n") for s in names]
    colors = ["#667788","#e69f00","#0072b2","#a07020","#2373b4"]
    ax = axes[0,0]
    bars = ax.bar(short,[axdata[s]["coverage"]*100 for s in names],color=colors)
    ax.set(ylim=(0,115),ylabel="Correct clicks / requests needing clicks (%)",
           title=f"Raw AX grounding • {axdata[names[0]]['n']//2} cases × 2 per arm")
    for b,s in zip(bars,names,strict=True):
        d=axdata[s]
        ax.text(b.get_x()+b.get_width()/2,b.get_height()+2,
                f"{d['coverage']:.0%}\n{d['wrong_clicks']} wrong",ha="center",fontsize=9)
    guarded=data["guarded_keyword_admission"]
    ax.scatter([0],[guarded["coverage"]*100],s=90,facecolors="white",edgecolors="black",zorder=5,
               label=f"Guarded router: {guarded['coverage']:.1%}, {len(guarded['wrong'])} wrong")
    ax.legend(loc="lower right",fontsize=9)
    ax = axes[0,1]
    ax.bar(short,[axdata[s]["p50_ms"] for s in names],color=colors)
    ax.scatter(short,[axdata[s]["p90_ms"] for s in names],color="#222222",marker="D",label="p90")
    ax.set(yscale="log",ylabel="Measured decision stage (ms; log scale)",
           title="AX stage latency • median bars, p90 diamonds")
    ax.legend()
    for i,s in enumerate(names):
        ax.annotate(f"{axdata[s]['p50_ms']:.3g} ms",(i,axdata[s]["p50_ms"]),xytext=(0,5),
                    textcoords="offset points",ha="center",fontsize=9)
    visual = data["visual"]
    namesv = list(visual)
    ax = axes[1,0]
    x=list(range(len(namesv)))
    ax.bar([i-.18 for i in x],[visual[s]["coverage"]*100 for s in namesv],width=.35,label="Correct target coverage",color="#0072b2")
    ax.bar([i+.18 for i in x],[visual[s]["true_positive_rate"]*100 for s in namesv],width=.35,label="Verified positive results",color="#009e73")
    ax.set(xticks=x,xticklabels=namesv,ylim=(0,115),ylabel="Percent",
           title="Authored image-dependent screens • 36 cases × 2/provider")
    ax.legend(loc="lower right",fontsize=9)
    for i,s in enumerate(namesv):
        d=visual[s]
        ax.text(i,105,f"{d['wrong_clicks']} wrong targets; {d['false_positive_rate']:.0%} false verification",ha="center",fontsize=9)
    ax = axes[1,1]
    ax.bar([i-.18 for i in x],[visual[s]["target_p50_ms"] for s in namesv],width=.35,label="Target median",color="#0072b2")
    ax.bar([i+.18 for i in x],[visual[s]["verification_p50_ms"] for s in namesv],width=.35,label="Verification median",color="#009e73")
    ax.set(xticks=x,xticklabels=namesv,ylabel="Decision call (ms)",title="Image slice latency • Jev has AX text only")
    ax.legend()
    bd=data["browser"]
    namesb=list(bd)
    xb=list(range(len(namesb)))
    ax = axes[2,0]
    ax.bar([i-.18 for i in xb],[100*bd[s]["goal_reached"]/bd[s]["n"] for s in namesb],width=.35,label="Goal observed",color="#9ccbc0")
    ax.bar([i+.18 for i in xb],[100*bd[s]["autonomous_success"]/bd[s]["n"] for s in namesb],width=.35,label="Finished autonomously",color="#009e73")
    ax.set(xticks=xb,xticklabels=namesb,ylim=(0,115),ylabel="Task success (%)",
           title="Plan once + Jev browser executor • 9 tasks × 2/model")
    ax.legend(loc="lower right",fontsize=9)
    for i,s in enumerate(namesb):
        ax.text(i,105,f"{bd[s]['handoffs']} handoffs; {bd[s]['errors']} errors",ha="center",fontsize=9)
    ax = axes[2,1]
    ax.bar(namesb,[bd[s]["p50_secs"] for s in namesb],color="#009e73")
    ax.scatter(namesb,[bd[s]["p90_secs"] for s in namesb],color="#222222",marker="D",label="p90")
    ax.set(ylabel="Whole task wall time (seconds)",title="Browser task latency • median bars, p90 diamonds")
    ax.legend()
    fig.supxlabel("Development fixtures and synthetic screens; thresholds unchanged. Raw grounding excludes router admission. AX stage ≠ whole task.\n"
                  "Hybrids reuse measured component calls. Direct desktop capture blocked; Codex fallback not freshly benchmarked.",fontsize=10)
    if "admission" in data:
        fig.supxlabel("Development fixtures; unchanged gates. Raw grounding excludes admission; see admission.png for actual guarded code.\n"
                      "AX stage differs from whole task. Decisions admission uses AX only with frozen observations and recording effectors.", fontsize=10)
    fig.savefig(folder/"comparison.png",dpi=160)
    fig.savefig(folder/"comparison.svg")
    normalize_svg(folder/"comparison.svg")
    plt.close(fig)
    if "admission" in data:
        draw_admission(folder, data["admission"])


def draw_admission(folder, admission):
    fig, axes = plt.subplots(1, 2, figsize=(12, 5), layout="constrained")
    names = list(admission)
    labels = [n.replace(" → ", " →\n") for n in names]
    bars = axes[0].bar(labels, [admission[n]["coverage"]*100 for n in names], color=["#667788", "#0072b2"])
    axes[0].set(ylim=(0, 115), ylabel="Correct admitted inputs / click requests (%)", title="Actual guarded admission code • frozen AX fixtures")
    for bar, name in zip(bars, names, strict=True):
        row = admission[name]
        axes[0].text(bar.get_x()+bar.get_width()/2, bar.get_height()+2,
                     f"{row['coverage']:.1%} coverage; {row['wrong_clicks']} wrong", ha="center")
    axes[1].bar(labels, [admission[n]["p50_ms"] for n in names], color=["#667788", "#0072b2"])
    axes[1].scatter(labels, [admission[n]["p90_ms"] for n in names], color="#222222", marker="D", label="p90")
    axes[1].set(yscale="log", ylabel="Measured local admission + target call (ms)", title="Median bars; p90 diamonds; no OS capture/input time")
    axes[1].legend()
    fig.supxlabel("Actual router and Decisions filters; Decisions target calls use matched filtered menus and AX evidence only.\n"
                  "Recording effectors; simulated stable observations; verification forced false. These are admitted inputs, never task completions.", fontsize=9)
    fig.savefig(folder/"admission.png", dpi=160)
    fig.savefig(folder/"admission.svg")
    normalize_svg(folder/"admission.svg")
    plt.close(fig)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("folder",type=Path,nargs="?")
    parser.add_argument("--check",type=Path)
    args=parser.parse_args()
    folder=args.check or args.folder
    if not folder:
        parser.error("supply output folder")
    data=summaries(folder)
    if args.check:
        assert json.loads((folder/"summary.json").read_text()) == data
        with Image.open(folder/"comparison.png") as im:
            assert im.width >= 1500 and im.height >= 1500
        assert "Raw AX grounding" in (folder/"comparison.svg").read_text()
        assert sum(d["n"] for d in data["browser"].values())==54
        if "admission" in data:
            with Image.open(folder/"admission.png") as im:
                assert im.width >= 1500 and im.height >= 700
            assert "never task completions" in (folder/"admission.svg").read_text()
        # Re-render from recomputed rows in a temporary directory. Dimensions and a chart
        # title alone cannot detect a stale graph after its source metrics change.
        with tempfile.TemporaryDirectory(prefix="eve-protocol-plot-check-") as temp:
            fresh = Path(temp)
            draw(fresh, data)
            for name in ("comparison.png", *(["admission.png"] if "admission" in data else [])):
                with Image.open(folder/name) as actual, Image.open(fresh/name) as expected:
                    assert actual.size == expected.size
                    assert ImageChops.difference(actual.convert("RGB"), expected.convert("RGB")).getbbox() is None, "stale or altered plot"
        print("PLOTS_CHECKED")
    else:
        (folder/"summary.json").write_text(json.dumps(data,indent=2)+"\n")
        draw(folder,data)


if __name__ == "__main__":
    main()

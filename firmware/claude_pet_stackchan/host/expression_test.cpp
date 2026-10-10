#include "../src/expression.h"
#include <cassert>
#include <cstdio>
int main() {
  using namespace expression;
  assert(parse("happy") == Happy && parse("invalid") == None && parse(nullptr) == None);
  // Every live label has a distinct shape, not just a new name.
  Kind live[] = {Calm, Happy, Curious, Affection, Surprised, Sad, Worried, Skeptical, Frustrated, Excited, Wink};
  for (auto k : live) {
    assert(parse(name(k)) == k);
    auto a = style(k);
    assert(a.left >= 24 && a.left <= 110 && a.right >= 24 && a.right <= 110);
    // Wink uses the normal rounded-square geometry; its closed curved lid is the distinct feature.
    for (auto other : live) if (other != k && k != Wink && other != Wink) {
      auto b = style(other);
      assert(a.left != b.left || a.right != b.right || a.width != b.width || a.radius != b.radius
          || a.mood != b.mood || a.curious != b.curious);
    }
  }
  auto winkStyle = style(Wink), normalStyle = style(Calm);
  assert(winkStyle.width == normalStyle.width && winkStyle.left == normalStyle.left
      && winkStyle.right == normalStyle.right && winkStyle.radius == normalStyle.radius);
  WinkHold wink;
  wink.start(100);
  assert(wink.active(99) && wink.active(749) && !wink.active(750));
  wink.start(0xffffff00);
  assert(wink.active(100) && !wink.active(500));
  assert(winkCurveY(0, 80) == 0 && winkCurveY(80, 80) == 0 && winkCurveY(40, 80) == -8);
  assert(winkCurveY(22, 91) == -8 && winkCurveY(69, 91) == -8);
  for (int x=0; x<=80; ++x) assert(winkCurveY(x,80) == winkCurveY(80-x,80));
  State s;
  assert(s.accept({1, Happy, 4000}, 100));
  assert(s.active(99) && s.active(4099) && !s.active(4100));
  assert(!s.accept({1, Curious, 4000}, 200));
  assert(!s.accept({0, Curious, 4000}, 200));
  assert(s.accept({5, None, 4000}, 11000) && !s.active(11000));
  assert(s.accept({6, Happy, 0}, 12000) && s.request.ttl == 500);
  assert(s.accept({7, Happy, 99999}, 12000) && s.request.ttl == 6000);
  assert(allowed(false, false, false, false, false));
  for (int i=0; i<5; i++) assert(!allowed(i==0,i==1,i==2,i==3,i==4));
  State wrap;
  assert(wrap.accept({0xfffffffe, Happy, 500}, 0xffffff00));
  assert(wrap.active(100) && !wrap.active(300));
  assert(wrap.accept({1, Curious, 4000}, 300));
  assert(!wrap.accept({0xfffffffe, Happy, 4000}, 301));
  puts("EXPRESSION_FIRMWARE_OK");
}

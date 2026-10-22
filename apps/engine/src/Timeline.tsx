// The timeline's elements: the bar (a canvas), the window over it with its
// handles, the playback position and the playback control. What they show,
// and how they move, is the controller's (engine-view.ts): the bar is
// drawn on the canvas as time passes.
import { Select, type Choice } from '@monokulo/ui/solid';

const choice = (value: string, label: string, is: { selected?: boolean; disabled?: boolean } = {}): Choice => ({
  value, label, text: label, current: false, selected: is.selected ?? false, disabled: is.disabled ?? false,
});

export function Timeline() {
  return (
    <section class="engine-card engine-timeline" id="engine-timeline" aria-label="Timeline">
      <div class="tl-track" id="tl-track">
        <canvas id="tl" aria-label="The whole history this page holds, each event a line and each key event a circle. A press outside the window takes the window and the playback position there." />
        <div
          class="tl-win" id="tl-win" tabindex="0" role="group"
          aria-label="The window: the stretch being looked at. Drag it to move it, a press in it goes to that moment. Left and right jump between key events, with Shift between any events; Space plays; End returns to live."
        >
          <span class="tl-handle l" id="tl-from" tabindex="0" role="slider" aria-label="Start of the window: left and right move it" />
          <span class="tl-handle r" id="tl-to" tabindex="0" role="slider" aria-label="End of the window: left and right move it" />
        </div>
        <div class="tl-head" id="tl-head" role="slider" aria-label="Playback position" />
        <div class="tl-axis" id="tl-axis" />
        <div class="tl-tip" id="tl-tipbox" hidden />
      </div>
      <div class="tl-modes" id="tl-modes">
        <label for="tl-mode">Playback</label>
        <Select
          name="tl-mode"
          id="tl-mode"
          compact={true}
          options={[
            choice('live', 'Live', { selected: true }),
            choice('replay', 'Play', { disabled: true }),
            choice('paused', 'Pause'),
          ]}
        />
        {/* How far behind live, under the control: it takes no room in the
            row, so the bar never changes width. */}
        <div class="tl-read" id="tl-read"><span id="tl-text" /></div>
      </div>
    </section>
  );
}

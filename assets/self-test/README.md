These synthetic, non-private speech fixtures were generated with eSpeak NG 1.52.0:

```sh
espeak-ng -v en-us -s 135 -w microphone.wav 'Alice speaking. We approved the release for Friday. The release is approved for Friday.'
espeak-ng -v en-gb -s 130 -w output.wav 'Morgan speaking. I will prepare the deployment checklist by Thursday. Morgan owns the deployment checklist.'
```

The acceptance harness plays them sequentially into separate virtual devices. Both
sources must survive production capture, Whisper, and local notes generation.
